//! Сбор метрик ресурсов запущенного процесса QEMU через `/proc`.
//!
//! Источники данных (все — файлы procfs, доступные без привилегий):
//!
//! | Метрика | Файл | Поле |
//! |---|---|---|
//! | CPU% | `/proc/<pid>/stat` + `/proc/uptime` | utime+stime delta / uptime delta |
//! | RAM (bytes) | `/proc/<pid>/status` | VmRSS |
//! | Disk read B/s | `/proc/<pid>/io` | read_bytes delta |
//! | Disk write B/s | `/proc/<pid>/io` | write_bytes delta |
//! | Net rx B/s | `/proc/<pid>/net/dev` | rx_bytes delta |
//! | Net tx B/s | `/proc/<pid>/net/dev` | tx_bytes delta |
//!
//! Все функции чтения — синхронные (`std::fs::read_to_string`), не `async`:
//! procfs-файлы мгновенны (единственный system call `read`), блокирование
//! tokio-воркера на микросекунды не имеет значения. Поллер запускается как
//! `tokio::spawn` с `tokio::task::spawn_blocking` внутри, если нужно.
//!
//! Паттерн вычисления CPU% — классический `delta(proc_time) / delta(uptime)`
//! (см. `man proc` раздел `/proc/[pid]/stat`). Без этого дельта-метода
//! значение CPU% будет некорректным на SMP и при不同的 процессорных частотах.

use andler_core::ResourceMetrics;

/// Интервал по умолчанию между выборками метрик.
pub const DEFAULT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Сырые значения CPU-таймеров процесса (utime + stime) и uptime системы
/// в тиках/секундах — промежуточное представление для вычисления CPU%.
#[derive(Debug, Clone, Copy)]
struct CpuSample {
    /// utime + stime из `/proc/<pid>/stat` (в тиках, ticks_per_sec).
    process_ticks: u64,
    /// uptime из `/proc/uptime` (в секундах с дробной частью).
    uptime_secs: f64,
    /// Количество ядер CPU (для нормализации).
    num_cpus: usize,
}

/// Сырые значения счётчиков I/O для вычисления дельты.
#[derive(Debug, Clone, Copy, Default)]
struct IoSample {
    /// read_bytes из `/proc/<pid>/io`.
    disk_read_bytes: u64,
    /// write_bytes из `/proc/<pid>/io`.
    disk_write_bytes: u64,
    /// rx_bytes из `/proc/<pid>/net/dev` (первая non-lo интерфейс).
    net_rx_bytes: u64,
    /// tx_bytes из `/proc/<pid>/net/dev` (первая non-lo интерфейс).
    net_tx_bytes: u64,
}

// ---------------------------------------------------------------------------
// /proc reader functions — pure, testable with mock data
// ---------------------------------------------------------------------------

/// Читает utime+stime (поля 14+15) из `/proc/<pid>/stat` и uptime из
/// `/proc/uptime`. Возвращает `None` при любой I/O-ошибке (процесс уже
/// завершился, нет доступа, некорректный формат).
///
/// Формат `/proc/<pid>/stat` (см. `man proc`):
/// ```text
/// pid (comm) state ppid pgroup session tty_nr tpgid flags minflt cminflt
/// majflt cmajflt utime stime cutime cstime priority nice num_threads
/// itrealvalue starttime ...
/// ```
/// Парсим `utime` (поле 14) и `stime` (поле 15) — 0-indexed: 13 и 14.
///
/// Формат `/proc/uptime`:
/// ```text
/// <uptime_secs> <idle_secs>
/// ```
fn read_proc_cpu(pid: u32) -> Option<CpuSample> {
    let stat_path = format!("/proc/{pid}/stat");
    let stat_content = std::fs::read_to_string(&stat_path).ok()?;

    // Поле 2 (comm) может содержать пробелы и скобки — пропускаем до
    // первого `)` после开场的 `pid (`:
    let after_comm = stat_content.rfind(')')?;
    let fields: Vec<&str> = stat_content[after_comm + 2..].split_whitespace().collect();

    // utime = поле 14 (1-indexed) = index 11 (после пропуска pid, comm)
    // stime = поле 15 (1-indexed) = index 12
    if fields.len() < 13 {
        return None;
    }
    let utime: u64 = fields[11].parse().ok()?;
    let stime: u64 = fields[12].parse().ok()?;

    let uptime_content = std::fs::read_to_string("/proc/uptime").ok()?;
    let uptime_secs: f64 = uptime_content.split_whitespace().next()?.parse().ok()?;

    // Количество ядер: читаем из /proc/stat (первая строка "cpu  ...")
    let stat_global = std::fs::read_to_string("/proc/stat").ok()?;
    let cpu_line = stat_global.lines().next()?;
    // Поле 0 = "cpu", поля 1..N = значения per-core, всего N-1 полей = число ядер
    let num_cpus = cpu_line.split_whitespace().count().saturating_sub(1).max(1);

    Some(CpuSample {
        process_ticks: utime + stime,
        uptime_secs,
        num_cpus,
    })
}

/// Читает VmRSS из `/proc/<pid>/status`. Возвращает `None` при ошибке.
///
/// Формат `/proc/<pid>/status` (см. `man proc`):
/// ```text
/// VmRSS:    12340 kB
/// ```
fn read_proc_rss(pid: u32) -> Option<u64> {
    let status_path = format!("/proc/{pid}/status");
    let content = std::fs::read_to_string(&status_path).ok()?;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Читает read_bytes и write_bytes из `/proc/<pid>/io`.
///
/// Формат `/proc/<pid>/io` (см. `man proc`):
/// ```text
/// rchar: 12345
/// wchar: 67890
/// syscr: 100
/// syscw: 200
/// read_bytes: 8192
/// write_bytes: 4096
/// ```
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

/// Читает rx_bytes и tx_bytes из `/proc/<pid>/net/dev` для первой
/// non-lo интерфейса.
///
/// Формат `/proc/<pid>/net/dev` (см. `man proc`):
/// ```text
/// Inter-|   Receive                                                |  Transmit
///  face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo frame compressed
///     lo: 12345    100    0    0    0     0          0         0   12345    100    0    0    0     0       0
///   eth0: 67890    200    0    0    0     0          0         0   67890    200    0    0    0     0       0
/// ```
fn read_proc_net(pid: u32) -> Option<(u64, u64)> {
    let net_path = format!("/proc/{pid}/net/dev");
    let content = std::fs::read_to_string(&net_path).ok()?;

    let mut lines = content.lines();
    // Первые две строки — заголовки
    lines.next()?;
    lines.next()?;

    for line in lines {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("lo:") {
            // Пропуска loopback — нам нужен реальный интерфейс
            let _ = rest;
            continue;
        }
        if let Some(rest) = trimmed.split_once(':') {
            let iface_data = rest.1.trim();
            let parts: Vec<&str> = iface_data.split_whitespace().collect();
            // rx_bytes = поле 0 (после :), tx_bytes = поле 8
            if parts.len() >= 9 {
                let rx: u64 = parts[0].parse().ok()?;
                let tx: u64 = parts[8].parse().ok()?;
                return Some((rx, tx));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Delta computation
// ---------------------------------------------------------------------------

/// Вычисляет CPU% из двух выборок. Возвращает `None` если дельта uptime
/// равна нулю (недостаточно данных для вычисления) или tcks_per_sec == 0.
fn compute_cpu_percent(prev: CpuSample, curr: CpuSample, ticks_per_sec: u64) -> Option<f32> {
    if ticks_per_sec == 0 {
        return None;
    }
    let delta_ticks = curr.process_ticks.saturating_sub(prev.process_ticks);
    let delta_secs = curr.uptime_secs - prev.uptime_secs;
    if delta_secs <= 0.0 {
        return None;
    }

    // CPU% = (delta_ticks / ticks_per_sec) / delta_secs / num_cpus * 100
    let cpu_secs = delta_ticks as f64 / ticks_per_sec as f64;
    let pct = (cpu_secs / delta_secs / curr.num_cpus as f64 * 100.0) as f32;
    Some(pct.clamp(0.0, 100.0 * curr.num_cpus as f32))
}

/// Вычисляет rates (B/s) из двух IO-сэмплов и интервала.
fn compute_io_rates(prev: IoSample, curr: IoSample, delta_secs: f64) -> (u64, u64, u64, u64) {
    if delta_secs <= 0.0 {
        return (0, 0, 0, 0);
    }
    let disk_read = ((curr.disk_read_bytes - prev.disk_read_bytes) as f64 / delta_secs) as u64;
    let disk_write = ((curr.disk_write_bytes - prev.disk_write_bytes) as f64 / delta_secs) as u64;
    let net_rx = ((curr.net_rx_bytes - prev.net_rx_bytes) as f64 / delta_secs) as u64;
    let net_tx = ((curr.net_tx_bytes - prev.net_tx_bytes) as f64 / delta_secs) as u64;
    (disk_read, disk_write, net_rx, net_tx)
}

// ---------------------------------------------------------------------------
// Public: one-shot sample
// ---------------------------------------------------------------------------

/// Считывает один снимок метрик для процесса с заданным PID.
/// Используется поллером внутри `spawn_metrics_poller`.
pub fn read_metrics_sample(pid: u32) -> Option<ResourceMetrics> {
    let _cpu = read_proc_cpu(pid)?;
    let rss = read_proc_rss(pid);
    let (disk_read, disk_write) = read_proc_io(pid).unwrap_or((0, 0));
    let (net_rx, net_tx) = read_proc_net(pid).unwrap_or((0, 0));

    Some(ResourceMetrics {
        cpu_percent: None, // Вычисляется при дельте, здесь — сырые данные
        memory_used_bytes: rss,
        disk_read_bytes_per_sec: Some(disk_read),
        disk_write_bytes_per_sec: Some(disk_write),
        net_rx_bytes_per_sec: Some(net_rx),
        net_tx_bytes_per_sec: Some(net_tx),
        ..ResourceMetrics::default()
    })
}

// ---------------------------------------------------------------------------
// Public: poller task
// ---------------------------------------------------------------------------

/// Запускает фоновую задачу, которая каждые `interval` секунд считывает
/// метрики из `/proc/<pid>/` и публикует их в `broadcast::Sender`.
///
/// Задача завершается, когда `sender` больше не имеет подписчиков
/// (все gRPC-клиенты отключились) или процесс с PID `pid` завершился.
///
/// `ticks_per_sec` — `sysconf(_SC_CLK_TCK)` (обычно 100 на Linux).
/// Передаётся извне, а не читается здесь, чтобы не делать вызов
/// `sysconf` при каждом тике и не зависеть от C- FFI вtokio-задаче.
pub fn spawn_metrics_poller(
    pid: u32,
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

            // Если процесс завершился — выходим
            let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
            if !alive {
                tracing::debug!(pid, "metrics poller: process exited, stopping");
                break;
            }

            // Если нет подписчиков — выходим (broadcast::senderRefCount == 1
            // означает, что только poller держит sender)
            if sender.receiver_count() == 0 {
                tracing::debug!(pid, "metrics poller: no subscribers, stopping");
                break;
            }

            let now = std::time::Instant::now();

            // Считываем данные из /proc
            let cpu_sample = read_proc_cpu(pid);
            let rss = read_proc_rss(pid);
            let (disk_read, disk_write) = read_proc_io(pid).unwrap_or((0, 0));
            let (net_rx, net_tx) = read_proc_net(pid).unwrap_or((0, 0));

            let io_sample = IoSample {
                disk_read_bytes: disk_read,
                disk_write_bytes: disk_write,
                net_rx_bytes: net_rx,
                net_tx_bytes: net_tx,
            };

            // Вычисляем CPU%
            let cpu_percent = if let (Some(prev), Some(curr)) = (prev_cpu, cpu_sample) {
                compute_cpu_percent(prev, curr, ticks_per_sec)
            } else {
                None
            };

            // Вычисляем I/O rates
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

            // GPU-метрики из sysfs (AMD только) — читаются каждую секунду
            // вместе с host-метриками и отправляются единым сообщением.
            let mut metrics = metrics;
            let gpu = crate::gpu_metrics::read_gpu_metrics();
            crate::gpu_metrics::merge_gpu_metrics(&mut metrics, &gpu);

            let _ = sender.send(metrics);

            prev_cpu = cpu_sample;
            prev_io = Some(io_sample);
            prev_time = Some(now);
        }

        tracing::debug!(pid, "metrics poller: task finished");
    })
}

/// Возвращает `sysconf(_SC_CLK_TCK)` — количество таймерных тиков в секунду.
/// На Linux это обычно 100 (HZ=100), но может быть 250 или 1000.
/// Используется для конвертации utime/stime из `/proc/<pid>/stat` в секунды.
pub fn ticks_per_second() -> u64 {
    // sysconf(_SC_CLK_TCK) — безопасно вызывать, возвращает константу.
    // Безопасно: не нарушает память, не блокирует, не аллоцирует.
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) as u64 }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpu_stat_basic() {
        // Формат: pid (comm) state ... utime stime ...
        // После `)` идёт: state ppid pgroup session tty_nr tpgid flags
        // minflt cminflt majflt cmajflt utime stime ...
        // utime=поле 14 (1-indexed), stime=поле 15 (1-indexed)
        // После пропуска pid+comm, индексы: utime=11, stime=12 (0-indexed)
        //
        //                 state ppid pgrp sess  tty  tpgid flags minf  cmif  majf  cmaj  utime stime
        let stat = "12345 (qemu-system-x86) S 1 1 0 0 -1 4194304 0 0 0 0 1000 500 0 0 20 0 1 0";
        let after_comm = stat.rfind(')').unwrap();
        let fields: Vec<&str> = stat[after_comm + 2..].split_whitespace().collect();
        assert_eq!(fields[11], "1000"); // utime
        assert_eq!(fields[12], "500");  // stime
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
        // delta_ticks=100, ticks_per_sec=100, cpu_secs=1.0, delta_secs=1.0
        // pct = 1.0 / 1.0 / 4 * 100 = 25.0
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
        assert_eq!(dr, 500);  // (2000-1000)/2
        assert_eq!(dw, 1000); // (4000-2000)/2
        assert_eq!(nr, 500);  // (1500-500)/2
        assert_eq!(nt, 600);  // (1800-600)/2
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
    fn rss_parsing_from_status_format() {
        let content = "Name: qemu-system-\nUmask: 0022\nState: S (sleeping)\nTgid: 12345\nNgid: 0\nPid: 12345\nPPid: 1\nTracerPid: 0\nUid: 0 0 0 0\nGid: 0 0 0 0\nFDSize: 256\nGroups: 0 \nNStgid: 12345\nNSpid: 12345\nNSpgid: 12345\nNSsid: 12345\nVmPeak: 12345678 kB\nVmSize: 12345678 kB\nVmLck: 0 kB\nVmPin: 0 kB\nVmHWM: 123456 kB\nVmRSS: 98765 kB\n";
        let rss_line = content.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
        let kb: u64 = rss_line
            .strip_prefix("VmRSS:")
            .unwrap()
            .trim()
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
            if let Some(_) = trimmed.strip_prefix("lo:") {
                continue;
            }
            if let Some(rest) = trimmed.split_once(':') {
                let parts: Vec<&str> = rest.1.trim().split_whitespace().collect();
                assert_eq!(parts[0], "67890"); // rx_bytes
                assert_eq!(parts[8], "54321"); // tx_bytes
                break;
            }
        }
    }
}
