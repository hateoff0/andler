//! Download and cache ARM translator archives.
//!
//! Downloads translator archives from GitHub, verifies MD5 checksums,
//! and extracts them into the local cache directory (`~/.andler/cache/arm-translators/`).

use std::path::PathBuf;

use andler_core::android_profile::ArmTranslator;

use crate::error::DiskError;
use crate::translator::{dir_name, resolve};

/// Ensures the translator is available locally, downloading if necessary.
///
/// Returns the path to the extracted translator directory.
/// Cache hit: returns immediately if the detect_file already exists.
pub async fn ensure_translator(
    translator: ArmTranslator,
    android_version: &str,
) -> Result<PathBuf, DiskError> {
    let info = resolve(translator);
    let cache_path = andler_core::paths::arm_translators_dir().join(dir_name(translator));

    // Cache hit: detect_file already present
    if !info.detect_file.is_empty() && cache_path.join(info.detect_file).exists() {
        return Ok(cache_path);
    }

    // Find URL for the requested android_version
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

    let bytes = download_file(url).await?;
    verify_md5(&bytes, expected_md5)?;
    extract_zip(&bytes, &cache_path)?;

    Ok(cache_path)
}

/// Downloads a file from the given URL.
async fn download_file(url: &str) -> Result<Vec<u8>, DiskError> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| DiskError::NbdSetupFailed(format!("failed to download {url}: {e}")))?;

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

/// Verifies MD5 checksum of downloaded bytes.
fn verify_md5(bytes: &[u8], expected: &str) -> Result<(), DiskError> {
    let result = format!("{:x}", md5::compute(bytes));

    if result != expected {
        return Err(DiskError::NbdSetupFailed(format!(
            "MD5 mismatch: expected {expected}, got {result}"
        )));
    }
    Ok(())
}

/// Extracts a ZIP archive into the target directory.
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
            let mut out = std::fs::File::create(&outpath).map_err(|e| {
                DiskError::NbdSetupFailed(format!("failed to create file: {e}"))
            })?;
            std::io::copy(&mut file, &mut out)
                .map_err(|e| DiskError::NbdSetupFailed(format!("failed to write file: {e}")))?;
        }
    }

    Ok(())
}
