use andler_core::ResourceMetrics;

pub const DEFAULT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
struct CpuSample {
    process_ticks: u64,

    uptime_secs: f64,

    num_cpus: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct IoSample {
    disk_read_bytes: u64,

    disk_write_bytes: u64,

    net_rx_bytes: u64,

    net_tx_bytes: u64,
}

fn read_proc_cpu(pid: u32) -> Option<CpuSample> {
    let stat_path = format!("/proc/{pid}/stat");
    let stat_content = std::fs::read_to_string(&stat_path).ok()?;

    let after_comm = stat_content.rfind(')')?;
    let fields: Vec<&str> = stat_content[after_comm + 2..].split_whitespace().collect();

    if fields.len() < 13 {
        return None;
    }
    let utime: u64 = fields[11].parse().ok()?;
    let stime: u64 = fields[12].parse().ok()?;

    let uptime_content = std::fs::read_to_string("/proc/uptime").ok()?;
    let uptime_secs: f64 = uptime_content.split_whitespace().next()?.parse().ok()?;

    let stat_global = std::fs::read_to_string("/proc/stat").ok()?;
    let cpu_line = stat_global.lines().next()?;
    let num_cpus = cpu_line.split_whitespace().count().saturating_sub(1).max(1);

    Some(CpuSample {
        process_ticks: utime + stime,
        uptime_secs,
        num_cpus,
    })
}

fn read_proc_rss(pid: u32) -> Option<u64> {
    let status_path = format!("/proc/{pid}/status");
    let content = std::fs::read_to_string(&status_path).ok()?;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn read_proc_io(pid: u32) -> Option<(u64, u64)> {
    let io_path = format!("/proc/{pid}/io");
    let content = std::fs::read_to_string(&io_path).ok()?;

    let mut read_bytes = None;
    let mut write_bytes = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("read_bytes:") {
            read_bytes = Some(rest.trim().parse::<u64>().ok()?);
        } else if let Some(rest) = line.strip_prefix("write_bytes:") {
            write_bytes = Some(rest.trim().parse::<u64>().ok()?);
        }
        if read_bytes.is_some() && write_bytes.is_some() {
            break;
        }
    }

    Some((read_bytes?, write_bytes?))
}

fn read_proc_net(pid: u32) -> Option<(u64, u64)> {
    let net_path = format!("/proc/{pid}/net/dev");
    let content = std::fs::read_to_string(&net_path).ok()?;

    let mut lines = content.lines();
    lines.next()?;
    lines.next()?;

    for line in lines {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("lo:") {
            let _ = rest;
            continue;
        }
        if let Some(rest) = trimmed.split_once(':') {
            let iface_data = rest.1.trim();
            let parts: Vec<&str> = iface_data.split_whitespace().collect();
            if parts.len() >= 9 {
                let rx: u64 = parts[0].parse().ok()?;
                let tx: u64 = parts[8].parse().ok()?;
                return Some((rx, tx));
            }
        }
    }
    None
}

fn compute_cpu_percent(prev: CpuSample, curr: CpuSample, ticks_per_sec: u64) -> Option<f32> {
    if ticks_per_sec == 0 {
        return None;
    }
    let delta_ticks = curr.process_ticks.saturating_sub(prev.process_ticks);
    let delta_secs = curr.uptime_secs - prev.uptime_secs;
    if delta_secs <= 0.0 {
        return None;
    }

    let cpu_secs = delta_ticks as f64 / ticks_per_sec as f64;
    let pct = (cpu_secs / delta_secs / curr.num_cpus as f64 * 100.0) as f32;
    Some(pct.clamp(0.0, 100.0 * curr.num_cpus as f32))
}

fn compute_io_rates(prev: IoSample, curr: IoSample, delta_secs: f64) -> (u64, u64, u64, u64) {
    if delta_secs <= 0.0 {
        return (0, 0, 0, 0);
    }
    // /proc/<pid>/io counters are not strictly monotonic (page-cache accounting
    // can go down); a decrease must clamp to zero, not panic on overflow.
    let disk_read =
        (curr.disk_read_bytes.saturating_sub(prev.disk_read_bytes) as f64 / delta_secs) as u64;
    let disk_write =
        (curr.disk_write_bytes.saturating_sub(prev.disk_write_bytes) as f64 / delta_secs) as u64;
    let net_rx = (curr.net_rx_bytes.saturating_sub(prev.net_rx_bytes) as f64 / delta_secs) as u64;
    let net_tx = (curr.net_tx_bytes.saturating_sub(prev.net_tx_bytes) as f64 / delta_secs) as u64;
    (disk_read, disk_write, net_rx, net_tx)
}

pub fn spawn_metrics_poller(
    pid: u32,
    pidfd: std::sync::Arc<crate::pidfd::PidFd>,
    ticks_per_sec: u64,
    interval: std::time::Duration,
    sender: tokio::sync::broadcast::Sender<ResourceMetrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut prev_cpu: Option<CpuSample> = None;
        let mut prev_io: Option<IoSample> = None;
        let mut prev_time: Option<std::time::Instant> = None;

        loop {
            tokio::time::sleep(interval).await;

            // pidfd tells us about the exact process instance — never a
            // reused PID — unlike a /proc/<pid> existence check.
            if pidfd.has_exited() {
                tracing::debug!(pid, "metrics poller: process exited, stopping");
                break;
            }

            let now = std::time::Instant::now();

            // All /proc reads (plus the GPU metrics query) are blocking syscalls that
            // must not run on the async runtime — collect them on a blocking thread.
            let sample = tokio::task::spawn_blocking(move || {
                let cpu_sample = read_proc_cpu(pid);
                let rss = read_proc_rss(pid);
                let (disk_read, disk_write) = read_proc_io(pid).unwrap_or((0, 0));
                let (net_rx, net_tx) = read_proc_net(pid).unwrap_or((0, 0));
                let gpu = andler_firmware::metrics::read_gpu_metrics();
                (cpu_sample, rss, disk_read, disk_write, net_rx, net_tx, gpu)
            })
            .await;
            let Ok((cpu_sample, rss, disk_read, disk_write, net_rx, net_tx, gpu)) = sample else {
                tracing::warn!(pid, "metrics poller: sample task failed, skipping tick");
                continue;
            };

            let io_sample = IoSample {
                disk_read_bytes: disk_read,
                disk_write_bytes: disk_write,
                net_rx_bytes: net_rx,
                net_tx_bytes: net_tx,
            };

            let cpu_percent = if let (Some(prev), Some(curr)) = (prev_cpu, cpu_sample) {
                compute_cpu_percent(prev, curr, ticks_per_sec)
            } else {
                None
            };

            let (disk_read_rate, disk_write_rate, net_rx_rate, net_tx_rate) =
                if let (Some(prev_io_s), Some(delta)) = (prev_io, prev_time) {
                    let delta_secs = now.duration_since(delta).as_secs_f64();
                    compute_io_rates(prev_io_s, io_sample, delta_secs)
                } else {
                    (0, 0, 0, 0)
                };

            let metrics = ResourceMetrics {
                cpu_percent,
                memory_used_bytes: rss,
                disk_read_bytes_per_sec: Some(disk_read_rate),
                disk_write_bytes_per_sec: Some(disk_write_rate),
                net_rx_bytes_per_sec: Some(net_rx_rate),
                net_tx_bytes_per_sec: Some(net_tx_rate),
                ..ResourceMetrics::default()
            };

            let mut metrics = metrics;
            andler_firmware::metrics::merge_gpu_metrics(&mut metrics, &gpu);

            let _ = sender.send(metrics);

            prev_cpu = cpu_sample;
            prev_io = Some(io_sample);
            prev_time = Some(now);
        }

        tracing::debug!(pid, "metrics poller: task finished");
    })
}

pub fn ticks_per_second() -> u64 {
    // SAFETY: sysconf(_SC_CLK_TCK) is a pure POSIX call — no pointers, no mutable state.
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpu_stat_basic() {
        let stat = "12345 (qemu-system-x86) S 1 1 0 0 -1 4194304 0 0 0 0 1000 500 0 0 20 0 1 0";
        let after_comm = stat.rfind(')').unwrap();
        let fields: Vec<&str> = stat[after_comm + 2..].split_whitespace().collect();
        assert_eq!(fields[11], "1000"); // utime
        assert_eq!(fields[12], "500"); // stime
    }

    #[test]
    fn compute_cpu_percent_basic() {
        let prev = CpuSample {
            process_ticks: 1000,
            uptime_secs: 10.0,
            num_cpus: 4,
        };
        let curr = CpuSample {
            process_ticks: 1100,
            uptime_secs: 11.0,
            num_cpus: 4,
        };
        let pct = compute_cpu_percent(prev, curr, 100).unwrap();
        assert!((pct - 25.0).abs() < 0.1);
    }

    #[test]
    fn compute_cpu_percent_zero_delta() {
        let sample = CpuSample {
            process_ticks: 1000,
            uptime_secs: 10.0,
            num_cpus: 1,
        };
        assert!(compute_cpu_percent(sample, sample, 100).is_none());
    }

    #[test]
    fn compute_io_rates_basic() {
        let prev = IoSample {
            disk_read_bytes: 1000,
            disk_write_bytes: 2000,
            net_rx_bytes: 500,
            net_tx_bytes: 600,
        };
        let curr = IoSample {
            disk_read_bytes: 2000,
            disk_write_bytes: 4000,
            net_rx_bytes: 1500,
            net_tx_bytes: 1800,
        };
        let (dr, dw, nr, nt) = compute_io_rates(prev, curr, 2.0);
        assert_eq!(dr, 500); // (2000-1000)/2
        assert_eq!(dw, 1000); // (4000-2000)/2
        assert_eq!(nr, 500); // (1500-500)/2
        assert_eq!(nt, 600); // (1800-600)/2
    }

    #[test]
    fn compute_io_rates_zero_delta_secs() {
        let prev = IoSample::default();
        let curr = IoSample {
            disk_read_bytes: 100,
            ..Default::default()
        };
        let (dr, dw, nr, nt) = compute_io_rates(prev, curr, 0.0);
        assert_eq!(dr, 0);
        assert_eq!(dw, 0);
        assert_eq!(nr, 0);
        assert_eq!(nt, 0);
    }

    #[test]
    fn compute_io_rates_clamps_decreasing_counters_to_zero() {
        // /proc/<pid>/io counters can go down (page-cache accounting); a
        // decrease must clamp to zero instead of panicking on u64 overflow.
        let prev = IoSample {
            disk_read_bytes: 2000,
            disk_write_bytes: 4000,
            net_rx_bytes: 1500,
            net_tx_bytes: 1800,
        };
        let curr = IoSample {
            disk_read_bytes: 1000,
            disk_write_bytes: 3000,
            net_rx_bytes: 2000,
            net_tx_bytes: 1700,
        };
        let (dr, dw, nr, nt) = compute_io_rates(prev, curr, 1.0);
        assert_eq!(dr, 0, "decreasing disk read counter must clamp to zero");
        assert_eq!(dw, 0, "decreasing disk write counter must clamp to zero");
        assert_eq!(nr, 500);
        assert_eq!(nt, 0, "decreasing net tx counter must clamp to zero");
    }

    #[test]
    fn rss_parsing_from_status_format() {
        let content = "Name: qemu-system-\nUmask: 0022\nState: S (sleeping)\nTgid: 12345\nNgid: 0\nPid: 12345\nPPid: 1\nTracerPid: 0\nUid: 0 0 0 0\nGid: 0 0 0 0\nFDSize: 256\nGroups: 0 \nNStgid: 12345\nNSpid: 12345\nNSpgid: 12345\nNSsid: 12345\nVmPeak: 12345678 kB\nVmSize: 12345678 kB\nVmLck: 0 kB\nVmPin: 0 kB\nVmHWM: 123456 kB\nVmRSS: 98765 kB\n";
        let rss_line = content.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
        let kb: u64 = rss_line
            .strip_prefix("VmRSS:")
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(kb, 98765);
        assert_eq!(kb * 1024, 98765 * 1024);
    }

    #[test]
    fn net_dev_parsing() {
        let content = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo frame compressed\n    lo: 12345    100    0    0    0     0          0         0   12345    100    0    0    0     0       0\n  eth0: 67890    200    0    0    0     0          0         0   54321    150    0    0    0     0       0\n";
        let mut lines = content.lines();
        lines.next(); // header 1
        lines.next(); // header 2

        for line in lines {
            let trimmed = line.trim_start();
            if trimmed.strip_prefix("lo:").is_some() {
                continue;
            }
            if let Some(rest) = trimmed.split_once(':') {
                let parts: Vec<&str> = rest.1.split_whitespace().collect();
                assert_eq!(parts[0], "67890"); // rx_bytes
                assert_eq!(parts[8], "54321"); // tx_bytes
                break;
            }
        }
    }
}
