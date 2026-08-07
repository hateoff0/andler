use std::path::{Path, PathBuf};

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

    #[test]
    fn extract_zip_flattens_repo_prebuilts_prefix() {
        use std::io::Write;
        use std::path::Path;

        let dir = std::env::temp_dir().join(format!(
            "andler_test_extract_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::FileOptions::default();
            writer
                .add_directory("vendor_repo-prebuilt-abc123/", opts)
                .unwrap();
            writer
                .add_directory("vendor_repo-prebuilt-abc123/prebuilts/", opts)
                .unwrap();
            writer
                .add_directory("vendor_repo-prebuilt-abc123/prebuilts/lib/", opts)
                .unwrap();
            writer
                .start_file(
                    "vendor_repo-prebuilt-abc123/prebuilts/lib/libndk_translation.so",
                    opts,
                )
                .unwrap();
            writer.write_all(b"not-a-real-elf").unwrap();
            writer
                .start_file("vendor_repo-prebuilt-abc123/README.md", opts)
                .unwrap();
            writer.write_all(b"junk").unwrap();
            writer.finish().unwrap();
        }

        extract_zip(&buf, &dir).unwrap();
        let cache_root = Path::new(&dir);
        assert!(
            cache_root.join("lib/libndk_translation.so").is_file(),
            "payload must be hoisted out of the prebuilts/ prefix"
        );
        assert!(
            !cache_root.join("vendor_repo-prebuilt-abc123").exists(),
            "repo wrapper dir must be removed"
        );
        assert!(
            !cache_root.join("README.md").exists(),
            "non-prebuilts repo junk must not leak into the cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flatten_prebuilts_does_not_touch_already_flat_payload_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_flatten_flat_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib/arm")).unwrap();
        std::fs::create_dir_all(dir.join("wrapper-abc/prebuilts/bin")).unwrap();
        std::fs::write(dir.join("lib/arm/cpuinfo.so"), b"payload").unwrap();
        std::fs::write(dir.join("wrapper-abc/prebuilts/bin/houdini"), b"bin").unwrap();

        flatten_prebuilts(&dir).unwrap();

        assert!(
            dir.join("lib/arm/cpuinfo.so").is_file(),
            "already-flat payload dirs must not be hoisted"
        );
        assert!(dir.join("bin/houdini").is_file(), "prebuilts hoisted");
        assert!(
            !dir.join("wrapper-abc/prebuilts").exists(),
            "wrapper dir removed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flatten_prebuilts_errors_on_collision_instead_of_silently_skipping() {
        let dir = std::env::temp_dir().join(format!(
            "andler_test_flatten_collision_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib/arm")).unwrap();
        std::fs::create_dir_all(dir.join("wrapper_prepo/prebuilts/lib")).unwrap();
        std::fs::write(dir.join("lib/arm/cpuinfo.so"), b"payload").unwrap();
        std::fs::write(dir.join("wrapper_prepo/prebuilts/lib/libndk.so"), b"new").unwrap();

        let err = flatten_prebuilts(&dir).unwrap_err();

        assert!(
            err.to_string().contains("remove the translator cache dir"),
            "collision must surface as an actionable error: {err}"
        );
        assert!(
            !dir.join("lib/libndk.so").exists(),
            "nothing may go live on a partial extract"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    flatten_prebuilts(target)?;

    Ok(())
}

// GitHub archive zips of the prebuilt repos wrap the payload in a single
// `<repo>-<commit>/prebuilts/` directory. The cache existence check and the
// install path joins expect the files directly under the cache root, so hoist
// the payload up and drop the wrapper. Directories without a `prebuilts/`
// subdir are already-flattened payload dirs (bin/, lib/, ...) and are left
// alone — hoisting their contents would corrupt the layout.
fn flatten_prebuilts(target: &Path) -> Result<(), DiskError> {
    let entries: Vec<PathBuf> = std::fs::read_dir(target)
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to read extract dir: {e}")))?
        .flatten()
        .map(|e| e.path())
        .collect();

    for dir in entries {
        if !dir.is_dir() {
            continue;
        }
        let payload = dir.join("prebuilts");
        if !payload.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&payload)
            .map_err(|e| DiskError::NbdSetupFailed(format!("failed to read extract dir {e}")))?
        {
            let entry = entry.map_err(|e| {
                DiskError::NbdSetupFailed(format!("failed to read extract entry: {e}"))
            })?;
            let dst = target.join(entry.file_name());
            if dst.exists() {
                return Err(DiskError::NbdSetupFailed(format!(
                    "extract target {} already contains {}; remove the translator \
                     cache dir and retry",
                    target.display(),
                    dst.display()
                )));
            }
            std::fs::rename(entry.path(), &dst).map_err(|e| {
                DiskError::NbdSetupFailed(format!("failed to move extract entry: {e}"))
            })?;
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| DiskError::NbdSetupFailed(format!("failed to clean extract dir: {e}")))?;
    }

    Ok(())
}
