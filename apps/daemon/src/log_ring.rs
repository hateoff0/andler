use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

const RING_CAPACITY: usize = 4096;

/// In-memory ring of daemon log lines plus a broadcast channel for
/// follow-mode. The tracing layer (init_tracing) pushes every event here;
/// `StreamDaemonLogs` serves a snapshot (+ since filter) or subscribes.
/// Bounded by design: the daemon log itself is the archive, this is only
/// the recent tail (4k lines ≈ a full debug session at default verbosity).
pub struct LogRing {
    buf: Mutex<VecDeque<(u64, String)>>,
    tx: broadcast::Sender<String>,
}

impl LogRing {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(1024);
        Arc::new(LogRing {
            buf: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            tx,
        })
    }

    pub fn push(&self, ts_ms: u64, line: String) {
        let mut buf = self.buf.lock().unwrap();
        if buf.len() == RING_CAPACITY {
            buf.pop_front();
        }
        let _ = self.tx.send(line.clone());
        buf.push_back((ts_ms, line));
    }

    /// Snapshot of lines with `ts_ms >= since_ms`, oldest first.
    pub fn snapshot(&self, since_ms: u64) -> Vec<(u64, String)> {
        self.buf
            .lock()
            .unwrap()
            .iter()
            .filter(|(ts, _)| *ts >= since_ms)
            .cloned()
            .collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

/// `MakeWriter` that tees every formatted line into stdout (as before) and
/// into a `LogRing`. Used as the tracing writer in `init_tracing`, so the
/// ring carries exactly what the daemon log shows — including the JSON
/// shape when `ANDLERD_LOG_FORMAT=json` is set.
#[derive(Clone)]
pub struct RingMakeWriter {
    ring: Arc<LogRing>,
}

impl RingMakeWriter {
    pub fn new(ring: Arc<LogRing>) -> Self {
        RingMakeWriter { ring }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RingMakeWriter {
    type Writer = RingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RingWriter {
            ring: self.ring.clone(),
            buf: String::new(),
            stdout: std::io::stdout(),
        }
    }
}

pub struct RingWriter {
    ring: Arc<LogRing>,
    buf: String,
    stdout: std::io::Stdout,
}

impl std::io::Write for RingWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // tracing's fmt layer never calls flush between events — it writes
        // through io::Write::write_fmt only. Split completed lines on '\n'
        // here instead, so the ring advances even without a flush.
        self.buf.push_str(&String::from_utf8_lossy(data));
        while let Some(idx) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=idx).collect();
            if !line.trim().is_empty() {
                let ts_ms = chrono::Utc::now().timestamp_millis() as u64;
                self.ring.push(ts_ms, line);
            }
        }
        self.stdout.write(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.stdout.flush();
        if !self.buf.is_empty() {
            let ts_ms = chrono::Utc::now().timestamp_millis() as u64;
            self.ring.push(ts_ms, std::mem::take(&mut self.buf));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_bounded_snapshot_and_filters_by_since() {
        let ring = LogRing::new();
        for i in 0..5000 {
            ring.push(i as u64, format!("line {i}"));
        }
        let snap = ring.snapshot(0);
        assert_eq!(snap.len(), RING_CAPACITY, "ring must stay bounded");
        assert_eq!(snap[0].1, "line 904", "oldest lines are dropped");
        assert_eq!(snap[RING_CAPACITY - 1].1, "line 4999");

        let since = ring.snapshot(4990);
        assert_eq!(since.len(), 10);
        assert_eq!(since[0].1, "line 4990");
    }

    #[test]
    fn subscribe_receives_pushed_lines() {
        let ring = LogRing::new();
        let mut rx = ring.subscribe();
        ring.push(1, "hello".to_string());
        ring.push(2, "world".to_string());
        assert_eq!(rx.try_recv().unwrap(), "hello");
        assert_eq!(rx.try_recv().unwrap(), "world");
    }

    #[test]
    fn writer_tees_into_ring_on_flush() {
        use std::io::Write;
        let ring = LogRing::new();
        let mut writer = RingWriter {
            ring: ring.clone(),
            buf: String::new(),
            stdout: std::io::stdout(),
        };
        writer.write_all(b"INFO test: hello\n").unwrap();
        writer.flush().unwrap();
        let snap = ring.snapshot(0);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1, "INFO test: hello\n");
    }

    #[test]
    fn writer_splits_complete_lines_without_flush() {
        use std::io::Write;
        let ring = LogRing::new();
        let mut writer = RingWriter {
            ring: ring.clone(),
            buf: String::new(),
            stdout: std::io::stdout(),
        };
        // fmt writes events as one write_fmt without flushing; the ring
        // must still capture the completed line.
        writer
            .write_all(b"INFO a: first\nINFO a: second\n")
            .unwrap();
        let snap = ring.snapshot(0);
        assert_eq!(snap.len(), 2, "both completed lines captured without flush");
        assert_eq!(snap[0].1, "INFO a: first\n");
        assert_eq!(snap[1].1, "INFO a: second\n");
    }
}
