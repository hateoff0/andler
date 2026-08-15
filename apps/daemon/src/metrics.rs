use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::Instrument;

const LATENCY_SAMPLES_PER_METHOD: usize = 4096;

/// Daemon-internal metrics (§9.1.8 / P27): RPC latency by method with
/// p50/p99, error counts by gRPC status code (the public face of the
/// ErrorKind categories), instance/supervisor counts, active operations,
/// and QMP reconnect count. Collected by a tonic layer in the server
/// stack; exported via `GetDaemonMetrics` and `andler doctor --metrics`.
#[derive(Default)]
pub struct DaemonMetrics {
    latency: Mutex<std::collections::HashMap<String, VecDeque<u64>>>,
    by_code: Mutex<std::collections::HashMap<String, u64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    pub latency: Vec<MethodLatency>,
    pub by_code: Vec<(String, u64)>,
    pub instance_count: usize,
    pub running_count: usize,
    pub active_ops: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodLatency {
    pub method: String,
    pub count: u64,
    pub p50_ms: u64,
    pub p99_ms: u64,
}

impl DaemonMetrics {
    pub fn record_rpc(&self, method: &str, elapsed: Duration, code: Option<tonic::Code>) {
        let ms = elapsed.as_millis() as u64;
        let mut latency = self.latency.lock().unwrap();
        let bucket = latency.entry(method.to_string()).or_default();
        if bucket.len() == LATENCY_SAMPLES_PER_METHOD {
            bucket.pop_front();
        }
        bucket.push_back(ms);

        let code = code
            .map(|c| format!("{c:?}"))
            .unwrap_or_else(|| "OK".to_string());
        *self.by_code.lock().unwrap().entry(code).or_insert(0) += 1;
    }

    pub fn snapshot(
        &self,
        instance_count: usize,
        running_count: usize,
        active_ops: usize,
    ) -> MetricsSnapshot {
        let latency = {
            let latency = self.latency.lock().unwrap();
            let mut out: Vec<MethodLatency> = latency
                .iter()
                .map(|(method, samples)| {
                    let mut sorted: Vec<u64> = samples.iter().copied().collect();
                    sorted.sort_unstable();
                    let p = |q: f64| {
                        if sorted.is_empty() {
                            0
                        } else {
                            sorted[((sorted.len() - 1) as f64 * q) as usize]
                        }
                    };
                    MethodLatency {
                        method: method.clone(),
                        count: samples.len() as u64,
                        p50_ms: p(0.50),
                        p99_ms: p(0.99),
                    }
                })
                .collect();
            out.sort_by_key(|m| std::cmp::Reverse(m.count));
            out
        };
        let mut by_code: Vec<(String, u64)> = self
            .by_code
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        by_code.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

        MetricsSnapshot {
            latency,
            by_code,
            instance_count,
            running_count,
            active_ops,
        }
    }
}

/// tower layer wrapping every gRPC call with timing and status capture.
#[derive(Clone)]
pub struct MetricsLayer {
    metrics: Arc<DaemonMetrics>,
}

impl MetricsLayer {
    pub fn new(metrics: Arc<DaemonMetrics>) -> Self {
        MetricsLayer { metrics }
    }
}

impl<S> tower::Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    metrics: Arc<DaemonMetrics>,
}

impl<S, ReqBody, ResBody> tower::Service<tonic::codegen::http::Request<ReqBody>>
    for MetricsService<S>
where
    S: tower::Service<
            tonic::codegen::http::Request<ReqBody>,
            Response = tonic::codegen::http::Response<ResBody>,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: tonic::codegen::http::Request<ReqBody>) -> Self::Future {
        let method = request.uri().path().to_string();
        let request_id = request
            .extensions()
            .get::<tonic::metadata::MetadataMap>()
            .and_then(|m| m.get("request_id"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        // §9.1.1: every event emitted while handling this request carries
        // the request_id (and method) via the span — the CLI stamps the
        // header, so one log line reconstructs CLI → RPC → operation.
        let span = tracing::info_span!("rpc", request_id = %request_id, method = %method);
        let start = Instant::now();
        let metrics = self.metrics.clone();
        let mut inner = self.inner.clone();
        Box::pin(
            async move {
                let response = inner.call(request).await;
                // tonic server services have Error = Infallible; gRPC status is
                // carried in the response's grpc-status header. Missing header
                // (or http-level success) = OK.
                let code = response.as_ref().ok().and_then(|r| {
                    r.headers()
                        .get("grpc-status")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i32>().ok())
                        .map(tonic::Code::from_i32)
                });
                metrics.record_rpc(&method, start.elapsed(), code);
                response
            }
            .instrument(span),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_computed_from_bounded_samples() {
        let metrics = DaemonMetrics::default();
        for i in 0..2000u64 {
            metrics.record_rpc(
                "/AndlerService/GetVersion",
                Duration::from_millis(i % 100),
                None,
            );
        }
        let snap = metrics.snapshot(1, 1, 2);
        assert_eq!(snap.latency.len(), 1);
        let entry = &snap.latency[0];
        assert_eq!(entry.method, "/AndlerService/GetVersion");
        assert_eq!(entry.count, 2000);
        assert_eq!(entry.p50_ms, 49, "median of 0..99 is 49");
        assert_eq!(entry.p99_ms, 98, "p99 of 0..99 is 98");
    }

    #[test]
    fn errors_are_counted_by_code() {
        let metrics = DaemonMetrics::default();
        metrics.record_rpc("/a", Duration::from_millis(1), Some(tonic::Code::NotFound));
        metrics.record_rpc("/a", Duration::from_millis(2), Some(tonic::Code::NotFound));
        metrics.record_rpc("/a", Duration::from_millis(3), None);
        let snap = metrics.snapshot(0, 0, 0);
        assert_eq!(snap.by_code.len(), 2);
        assert!(snap.by_code.contains(&("NotFound".to_string(), 2)));
        assert!(snap.by_code.contains(&("OK".to_string(), 1)));
    }

    #[test]
    fn snapshot_reports_passed_counts() {
        let metrics = DaemonMetrics::default();
        let snap = metrics.snapshot(3, 1, 2);
        assert_eq!(snap.instance_count, 3);
        assert_eq!(snap.running_count, 1);
        assert_eq!(snap.active_ops, 2);
    }
}
