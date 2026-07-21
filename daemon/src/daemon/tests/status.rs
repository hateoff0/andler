use super::common::*;
use super::*;
use futures_util::StreamExt;

#[tokio::test]
async fn status_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon.status(InstanceId::new()).await.unwrap_err();
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}


#[tokio::test]
async fn stream_logs_before_start_returns_empty_stream_not_error() {
    let daemon = Daemon::new();
    let cfg = sample_config();
    let id = cfg.id;
    daemon.create_instance(cfg).await.unwrap();

    let mut stream = daemon.stream_instance_logs(id).await.unwrap();
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn stream_logs_on_unknown_instance_returns_instance_not_found() {
    let daemon = Daemon::new();
    let err = daemon
        .stream_instance_logs(InstanceId::new())
        .await
        .err()
        .expect("an unregistered instance_id must yield an error, not a stream");
    assert!(matches!(err, DaemonError::InstanceNotFound(_)));
}
