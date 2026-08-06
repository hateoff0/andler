use std::path::PathBuf;

use andler_core::android_profile::ArmTranslator;

use crate::error::DiskError;
use crate::translator::{dir_name, resolve};

pub async fn ensure_translator(
    translator: ArmTranslator,
    android_version: &str,
) -> Result<PathBuf, DiskError> {
    let info = resolve(translator);
    let cache_path = andler_core::paths::arm_translators_dir().join(dir_name(translator));

    if !info.detect_file.is_empty() && cache_path.join(info.detect_file).exists() {
        return Ok(cache_path);
    }

    let (url, expected_md5) = info
        .dl_links
        .iter()
        .find(|(ver, _, _)| *ver == android_version)
        .map(|(_, url, md5)| (*url, *md5))
        .ok_or_else(|| {
            DiskError::NbdSetupFailed(format!(
                "no download available for {translator:?} android {android_version}"
            ))
        })?;

    tracing::info!(
        translator = ?translator,
        android_version,
        url,
        "downloading ARM translator into {}",
        cache_path.display()
    );
    let bytes = download_file(url).await?;
    tracing::info!(translator = ?translator, bytes = bytes.len(), "translator download complete, verifying md5");
    verify_md5(&bytes, expected_md5)?;
    tracing::info!(translator = ?translator, "translator md5 verified, extracting");
    extract_zip(&bytes, &cache_path)?;

    Ok(cache_path)
}

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

async fn download_file(url: &str) -> Result<Vec<u8>, DiskError> {
    download_file_with_timeouts(url, CONNECT_TIMEOUT, TOTAL_TIMEOUT).await
}

async fn download_file_with_timeouts(
    url: &str,
    connect_timeout: std::time::Duration,
    total_timeout: std::time::Duration,
) -> Result<Vec<u8>, DiskError> {
    let client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(total_timeout)
        .build()
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to build http client: {e}")))?;

    let response = client.get(url).send().await.map_err(|e| {
        DiskError::NbdSetupFailed(format!(
            "failed to download {url}: {e}; check your network connection or \
                 pass --translator-dir <path> with a local copy of the translator"
        ))
    })?;

    if !response.status().is_success() {
        return Err(DiskError::NbdSetupFailed(format!(
            "download failed for {url}: HTTP {}",
            response.status()
        )));
    }

    response
        .bytes()
        .await
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to read download: {e}")))
        .map(|b| b.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn silent_server() -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("bound addr");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let (mut _stream, _) = listener.accept().expect("accept");
            let _ = rx.blocking_recv();
        });
        (addr, tx)
    }

    #[tokio::test]
    async fn download_times_out_on_silent_server_instead_of_hanging_forever() {
        let (addr, release) = silent_server().await;
        let url = format!("http://{addr}/translator.zip");
        let started = std::time::Instant::now();
        let err = download_file_with_timeouts(
            &url,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        let _ = release.send(());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "download must fail fast on a silent server, took {:?}",
            started.elapsed()
        );
        assert!(
            err.to_string().contains("--translator-dir"),
            "error must point at the --translator-dir workaround: {err}"
        );
    }

    #[tokio::test]
    async fn download_reports_http_error_status() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("bound addr");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            let _ = stream.flush();
            let _ = rx.blocking_recv();
        });
        let url = format!("http://{addr}/missing.zip");
        let err = download_file_with_timeouts(
            &url,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        let _ = tx.send(());
        assert!(err.to_string().contains("404"), "got: {err}");
    }
}

fn verify_md5(bytes: &[u8], expected: &str) -> Result<(), DiskError> {
    let result = format!("{:x}", md5::compute(bytes));

    if result != expected {
        return Err(DiskError::NbdSetupFailed(format!(
            "MD5 mismatch: expected {expected}, got {result}"
        )));
    }
    Ok(())
}

fn extract_zip(bytes: &[u8], target: &PathBuf) -> Result<(), DiskError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to open zip: {e}")))?;

    std::fs::create_dir_all(target)
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to create dir: {e}")))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| DiskError::NbdSetupFailed(format!("failed to read zip entry: {e}")))?;

        let outpath = target.join(file.mangled_name());

        if file.is_dir() {
            std::fs::create_dir_all(&outpath)
                .map_err(|e| DiskError::NbdSetupFailed(format!("failed to create dir: {e}")))?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DiskError::NbdSetupFailed(format!("failed to create parent dir: {e}"))
                })?;
            }
            let mut out = std::fs::File::create(&outpath)
                .map_err(|e| DiskError::NbdSetupFailed(format!("failed to create file: {e}")))?;
            std::io::copy(&mut file, &mut out)
                .map_err(|e| DiskError::NbdSetupFailed(format!("failed to write file: {e}")))?;
        }
    }

    Ok(())
}
