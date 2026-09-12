use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use andler_core::base_image::{self, ImageManifest};

use crate::error::DiskError;

pub const DEFAULT_REPO: &str = "hateoff0/andler";
pub const REPO_ENV: &str = "ANDLERD_IMAGE_REPO";
pub const API_BASE_ENV: &str = "ANDLERD_IMAGE_API_BASE";

const DEFAULT_API_BASE: &str = "https://api.github.com";
const RELEASE_TAG_PREFIX: &str = "base-image-android";
const MANIFEST_SUFFIX: &str = ".manifest.json";
const USER_AGENT: &str = "andler";

/// Newest-first releases are scanned until this many carry the release-tag
/// prefix; every scanned release costs one manifest GET, so the cap keeps
/// `andler image list --remote` from issuing a request per historical build.
const MAX_SCANNED_RELEASES: usize = 30;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-read stall guard, not a total budget: a multi-GB asset legitimately
/// takes minutes, but no single read may stall longer than this.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u32 = 3;
const PROGRESS_STEP_BYTES: u64 = 8 * 1024 * 1024;

/// Where published base images come from. Defaults suit the project's own
/// release pipeline; both halves are overridable so a mirror (or a fixture
/// server in tests) can serve the same layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSource {
    pub repo: String,
    pub api_base: String,
}

impl Default for ImageSource {
    fn default() -> Self {
        Self::new(DEFAULT_REPO, DEFAULT_API_BASE)
    }
}

impl ImageSource {
    pub fn new(repo: impl Into<String>, api_base: impl Into<String>) -> Self {
        let mut api_base: String = api_base.into();
        while api_base.ends_with('/') {
            api_base.pop();
        }
        Self {
            repo: repo.into(),
            api_base,
        }
    }

    pub fn from_env() -> Self {
        let repo = std::env::var(REPO_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_REPO.to_string());
        let api_base = std::env::var(API_BASE_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Self::new(repo, api_base)
    }

    /// Human-readable source label for CLI output and error text.
    pub fn describe(&self) -> String {
        format!("{} (api {})", self.repo, self.api_base)
    }

    pub fn release_page(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    fn releases_url(&self) -> String {
        format!("{}/repos/{}/releases", self.api_base, self.repo)
    }

    fn client(&self) -> Result<reqwest::Client, DiskError> {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| DiskError::ImageIndex {
                url: self.releases_url(),
                message: format!("failed to build http client: {e}"),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAsset {
    pub name: String,
    pub url: String,
    pub size_bytes: u64,
}

/// How a release ships its qcow2. The pipeline splits a zstd stream into
/// `.part` assets; a release that publishes a single asset works too, so a
/// manual upload does not need the splitting step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Parts of one zstd stream, in manifest order.
    Parts(Vec<RemoteAsset>),
    /// One zstd-compressed qcow2.
    Zstd(RemoteAsset),
    /// An uncompressed qcow2.
    Qcow2(RemoteAsset),
}

impl Payload {
    fn assets(&self) -> &[RemoteAsset] {
        match self {
            Payload::Parts(assets) => assets,
            Payload::Zstd(asset) | Payload::Qcow2(asset) => std::slice::from_ref(asset),
        }
    }

    fn is_compressed(&self) -> bool {
        matches!(self, Payload::Parts(_) | Payload::Zstd(_))
    }

    fn download_bytes(&self) -> u64 {
        self.assets().iter().map(|asset| asset.size_bytes).sum()
    }
}

/// One installable image as published in a GitHub release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    pub release_tag: String,
    /// Manifest asset name minus `.manifest.json` — the stem every payload
    /// asset shares and the name the image is installed under.
    pub stem: String,
    pub manifest: ImageManifest,
    /// The manifest exactly as published, so the installed copy keeps the
    /// fields andler does not model (source_image, git_rev, android_images).
    pub manifest_raw: Vec<u8>,
    pub payload: Payload,
}

impl RemoteImage {
    pub fn id(&self) -> String {
        self.manifest.id()
    }

    pub fn android_major(&self) -> &str {
        &self.manifest.android_major
    }

    pub fn android_variant(&self) -> &str {
        &self.manifest.android_variant
    }

    pub fn built_at(&self) -> &str {
        &self.manifest.built_at
    }

    pub fn download_bytes(&self) -> u64 {
        self.payload.download_bytes()
    }

    /// Size of the installed qcow2 when the manifest records it.
    pub fn installed_bytes(&self) -> Option<u64> {
        self.manifest.file_size_bytes
    }

    pub fn asset_count(&self) -> usize {
        self.payload.assets().len()
    }

    pub fn qcow2_sha256(&self) -> Option<&str> {
        self.manifest.sha256.as_deref()
    }

    /// The install path this image would land on if downloaded now.
    pub fn install_path(&self) -> PathBuf {
        base_images_dir_for(&self.manifest).join(format!("{}.qcow2", self.stem))
    }
}

/// Optional `(android major, package set)` filter for the remote catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFilter {
    pub android_major: Option<String>,
    pub android_variant: Option<String>,
}

impl ImageFilter {
    pub fn matches(&self, manifest: &ImageManifest) -> bool {
        if let Some(major) = &self.android_major {
            if &manifest.android_major != major {
                return false;
            }
        }
        if let Some(variant) = &self.android_variant {
            if !manifest.android_variant.eq_ignore_ascii_case(variant) {
                return false;
            }
        }
        true
    }

    pub fn describe(&self) -> String {
        match (&self.android_major, &self.android_variant) {
            (Some(major), Some(variant)) => format!("Android {major} {variant}"),
            (Some(major), None) => format!("Android {major}"),
            (None, Some(variant)) => variant.clone(),
            (None, None) => "any Android version".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchPhase {
    Downloading,
    Verifying,
    Extracting,
    Installing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchProgress {
    pub phase: FetchPhase,
    /// Asset being worked on (its published name), or the image stem while
    /// the payload is being unpacked.
    pub asset: String,
    pub asset_index: u32,
    pub asset_count: u32,
    /// Bytes fetched so far across the whole download (or written out while
    /// extracting), against [`FetchProgress::total_bytes`].
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// Shared progress callback. Clonable because unpacking runs on the blocking
/// pool while the download loop runs on the async runtime, and both report
/// into the same channel-backed sink.
#[derive(Clone)]
pub struct ProgressSink(std::sync::Arc<std::sync::Mutex<dyn FnMut(FetchProgress) + Send>>);

impl ProgressSink {
    pub fn new(report: impl FnMut(FetchProgress) + Send + 'static) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(report)))
    }

    /// A sink that drops every event, for callers that only want the outcome.
    pub fn silent() -> Self {
        Self::new(|_| {})
    }

    pub fn emit(&self, progress: FetchProgress) {
        let mut report = self.0.lock().unwrap_or_else(|e| e.into_inner());
        report(progress);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    pub qcow2_path: PathBuf,
    pub manifest_path: PathBuf,
    /// True when the already-cached build matched and nothing was downloaded.
    pub reused: bool,
}

impl RemoteImage {
    /// True when this exact build is already in the cache.
    pub fn is_installed(&self) -> bool {
        installed_image(&self.id()).is_some()
    }
}

/// The cached image whose manifest carries `manifest_id`, if the cache holds
/// one. Identity is the build label (version, variant, built_at) — a rebuild
/// under the same label is indistinguishable here, exactly as instance pins
/// treat it.
pub fn installed_image(manifest_id: &str) -> Option<base_image::BaseImageInfo> {
    let images = base_image::list_all().ok()?;
    images
        .into_iter()
        .find(|info| info.manifest().is_some_and(|m| m.id() == manifest_id))
}

fn base_images_dir_for(manifest: &ImageManifest) -> PathBuf {
    andler_core::paths::base_images_dir().join(base_image::install_subdir(
        &manifest.android_major,
        &manifest.android_variant,
    ))
}

#[derive(Debug, serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The published catalog, newest build per (Android version, package set)
/// first. Failures to *scan* one release (unreadable manifest, missing part
/// asset) skip that release with a warning rather than failing the listing:
/// a single incomplete publication must not hide the healthy ones.
pub async fn list_remote(
    source: &ImageSource,
    filter: &ImageFilter,
) -> Result<Vec<RemoteImage>, DiskError> {
    let client = source.client()?;
    let url = format!("{}?per_page=100", source.releases_url());
    let raw = get_bytes(&client, &url, "release index").await?;
    let releases: Vec<GithubRelease> =
        serde_json::from_slice(&raw).map_err(|e| DiskError::ImageIndex {
            url: url.clone(),
            message: format!("cannot parse the release index: {e}"),
        })?;

    let mut images: Vec<RemoteImage> = Vec::new();
    let mut scanned = 0usize;
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        if !release.tag_name.starts_with(RELEASE_TAG_PREFIX) {
            continue;
        }
        if scanned >= MAX_SCANNED_RELEASES {
            break;
        }
        scanned += 1;

        match remote_image_from_release(&client, &release).await {
            Ok(Some(image)) if filter.matches(&image.manifest) => images.push(image),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                release = %release.tag_name,
                error = %e,
                "skipping release: its published assets are incomplete"
            ),
        }
    }

    // Newest first, one entry per (major, variant).
    images.sort_by(|a, b| b.built_at().cmp(a.built_at()));
    let mut seen: Vec<(String, String)> = Vec::new();
    images.retain(|image| {
        let key = (
            image.manifest.android_major.clone(),
            image.manifest.android_variant.to_uppercase(),
        );
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    Ok(images)
}

async fn remote_image_from_release(
    client: &reqwest::Client,
    release: &GithubRelease,
) -> Result<Option<RemoteImage>, DiskError> {
    let Some(manifest_asset) = release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(MANIFEST_SUFFIX))
    else {
        return Ok(None);
    };
    let stem = manifest_asset
        .name
        .strip_suffix(MANIFEST_SUFFIX)
        .unwrap_or(&manifest_asset.name)
        .to_string();

    let raw = get_bytes(client, &manifest_asset.browser_download_url, "manifest").await?;
    let manifest = base_image::parse_manifest(&raw).map_err(|e| DiskError::ImageIndex {
        url: manifest_asset.browser_download_url.clone(),
        message: format!("release {}: {e}", release.tag_name),
    })?;

    let payload = if manifest.parts.is_empty() {
        single_asset_payload(release, &stem)?
    } else {
        let mut assets = Vec::with_capacity(manifest.parts.len());
        for part in &manifest.parts {
            let asset = release
                .assets
                .iter()
                .find(|asset| asset.name == part.name)
                .ok_or_else(|| DiskError::ImageIndex {
                    url: release.tag_name.clone(),
                    message: format!(
                        "manifest lists part `{}` but the release has no such asset",
                        part.name
                    ),
                })?;
            assets.push(RemoteAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size_bytes: if asset.size == 0 {
                    part.size_bytes
                } else {
                    asset.size
                },
            });
        }
        Payload::Parts(assets)
    };

    Ok(Some(RemoteImage {
        release_tag: release.tag_name.clone(),
        stem,
        manifest,
        manifest_raw: raw,
        payload,
    }))
}

fn single_asset_payload(release: &GithubRelease, stem: &str) -> Result<Payload, DiskError> {
    let find = |name: String| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| RemoteAsset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
                size_bytes: asset.size,
            })
    };

    if let Some(asset) = find(format!("{stem}.qcow2.zst")) {
        return Ok(Payload::Zstd(asset));
    }
    if let Some(asset) = find(format!("{stem}.qcow2")) {
        return Ok(Payload::Qcow2(asset));
    }
    Err(DiskError::ImageIndex {
        url: release.tag_name.clone(),
        message: format!("release has {stem}.manifest.json but no matching .qcow2 asset"),
    })
}

/// Downloads one published image into the local cache and returns where it
/// landed. Existing parts of an interrupted download are verified and reused,
/// so a retry resumes instead of pulling the whole image again.
pub async fn fetch_base_image(
    source: &ImageSource,
    image: &RemoteImage,
    force: bool,
    progress: &ProgressSink,
) -> Result<FetchOutcome, DiskError> {
    if image.qcow2_sha256().is_none() && matches!(image.payload, Payload::Qcow2(_)) {
        return Err(DiskError::ImageVerify {
            message: format!(
                "release {} ships an uncompressed qcow2 without a sha256 in its manifest; \
                 refusing to install an image that cannot be verified",
                image.release_tag
            ),
        });
    }

    let target_dir = base_images_dir_for(&image.manifest);
    std::fs::create_dir_all(&target_dir).map_err(|source| DiskError::Io {
        path: target_dir.clone(),
        source,
    })?;

    let qcow2_path = target_dir.join(format!("{}.qcow2", image.stem));
    let manifest_path = base_image::manifest_path_for(&qcow2_path);
    if !force
        && qcow2_path.exists()
        && base_image::read_manifest(&manifest_path)
            .map(|local| local.id() == image.id())
            .unwrap_or(false)
    {
        return Ok(FetchOutcome {
            qcow2_path,
            manifest_path,
            reused: true,
        });
    }

    let staging = target_dir.join(format!(".fetch-{}", image.stem));
    std::fs::create_dir_all(&staging).map_err(|source| DiskError::Io {
        path: staging.clone(),
        source,
    })?;

    let uncompressed_bytes = image.installed_bytes().unwrap_or(0);
    let required = image.download_bytes().saturating_add(uncompressed_bytes);
    crate::diskspace::check_available_space(&target_dir, required)?;

    let client = source.client()?;
    let outcome = fetch_into(&client, image, &staging, &qcow2_path, progress).await;
    let remove = std::fs::remove_dir_all(&staging);
    match (outcome, remove) {
        (Ok(()), Ok(())) => {}
        (Ok(()), Err(e)) => tracing::warn!(
            staging = %staging.display(),
            error = %e,
            "downloaded image installed, but its retry-scratch directory could not be removed"
        ),
        (Err(e), Ok(())) => return Err(e),
        (Err(e), Err(cleanup)) => {
            // The staging dir is only scratch space, but a silent failure to
            // remove it would leave multi-GB leftovers nobody notices.
            tracing::warn!(
                staging = %staging.display(),
                error = %cleanup,
                "download failed and its scratch directory could not be removed"
            );
            return Err(e);
        }
    }

    Ok(FetchOutcome {
        qcow2_path,
        manifest_path,
        reused: false,
    })
}

async fn fetch_into(
    client: &reqwest::Client,
    image: &RemoteImage,
    staging: &Path,
    qcow2_path: &Path,
    progress: &ProgressSink,
) -> Result<(), DiskError> {
    let assets = image.payload.assets();
    let total = match &image.payload {
        Payload::Parts(_) => image.payload.download_bytes(),
        Payload::Zstd(asset) | Payload::Qcow2(asset) => asset.size_bytes,
    };
    let mut downloaded: u64 = 0;

    for (index, asset) in assets.iter().enumerate() {
        let local = staging.join(&asset.name);
        let expected = image
            .manifest
            .parts
            .iter()
            .find(|part| part.name == asset.name)
            .map(|part| part.sha256.as_str());

        let local_ok = local
            .metadata()
            .map(|meta| meta.len() == asset.size_bytes && asset.size_bytes > 0)
            .unwrap_or(false)
            && match expected {
                Some(sha) => tokio::task::spawn_blocking({
                    let local = local.clone();
                    move || sha256_of_file(&local)
                })
                .await
                .map_err(|e| DiskError::ImageDownload {
                    asset: asset.name.clone(),
                    message: format!("hash task panicked: {e}"),
                })?
                .map(|actual| actual == sha)
                .unwrap_or(false),
                None => false,
            };

        if !local_ok {
            progress.emit(FetchProgress {
                phase: FetchPhase::Downloading,
                asset: asset.name.clone(),
                asset_index: index as u32 + 1,
                asset_count: assets.len() as u32,
                downloaded_bytes: downloaded,
                total_bytes: total,
            });
            download_asset(client, asset, &local, expected, downloaded, total, progress).await?;
        }
        downloaded += asset.size_bytes;
    }

    progress.emit(FetchProgress {
        phase: FetchPhase::Verifying,
        asset: image.stem.clone(),
        asset_index: assets.len() as u32,
        asset_count: assets.len() as u32,
        downloaded_bytes: downloaded,
        total_bytes: total,
    });

    let mut staging_files = Vec::with_capacity(assets.len());
    for asset in assets {
        let file =
            std::fs::File::open(staging.join(&asset.name)).map_err(|source| DiskError::Io {
                path: staging.join(&asset.name),
                source,
            })?;
        staging_files.push(file);
    }

    let partial = staging.join(format!("{}.qcow2.partial", image.stem));
    let expected_qcow2 = image.qcow2_sha256().map(|sha| sha.to_string());
    let uncompressed_total = image.installed_bytes().unwrap_or(0);
    let unpack = {
        let partial = partial.clone();
        let compressed = image.payload.is_compressed();
        let progress = progress.clone();
        tokio::task::spawn_blocking(move || {
            extract_qcow2(
                staging_files,
                compressed,
                &partial,
                expected_qcow2.as_deref(),
                uncompressed_total,
                &progress,
            )
        })
        .await
        .map_err(|e| DiskError::ImageDownload {
            asset: image.stem.clone(),
            message: format!("extract task panicked: {e}"),
        })?
    };
    unpack?;

    progress.emit(FetchProgress {
        phase: FetchPhase::Installing,
        asset: image.stem.clone(),
        asset_index: assets.len() as u32,
        asset_count: assets.len() as u32,
        downloaded_bytes: uncompressed_total,
        total_bytes: uncompressed_total,
    });

    let manifest_tmp = staging.join(format!("{}.manifest.json", image.stem));
    std::fs::write(&manifest_tmp, &image.manifest_raw).map_err(|source| DiskError::Io {
        path: manifest_tmp.clone(),
        source,
    })?;
    std::fs::rename(&manifest_tmp, base_image::manifest_path_for(qcow2_path)).map_err(
        |source| DiskError::Io {
            path: base_image::manifest_path_for(qcow2_path),
            source,
        },
    )?;
    std::fs::rename(&partial, qcow2_path).map_err(|source| DiskError::Io {
        path: qcow2_path.to_path_buf(),
        source,
    })?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn extract_qcow2(
    files: Vec<std::fs::File>,
    compressed: bool,
    partial: &Path,
    expected_sha256: Option<&str>,
    uncompressed_total: u64,
    progress: &ProgressSink,
) -> Result<(), DiskError> {
    use sha2::Digest;

    let reader = ChainedReader::new(files);
    let mut source: Box<dyn Read> = if compressed {
        Box::new(zstd::stream::read::Decoder::new(reader).map_err(|e| {
            DiskError::ImageDownload {
                asset: partial
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                message: format!("cannot open the compressed payload: {e}"),
            }
        })?)
    } else {
        Box::new(reader)
    };

    let file = std::fs::File::create(partial).map_err(|source| DiskError::Io {
        path: partial.to_path_buf(),
        source,
    })?;
    let mut out = std::io::BufWriter::with_capacity(4 * 1024 * 1024, file);
    let mut hasher = sha2::Sha256::new();
    let mut written: u64 = 0;
    let mut last_report: u64 = 0;
    let mut buf = vec![0u8; 4 * 1024 * 1024];

    loop {
        let read = source
            .read(&mut buf)
            .map_err(|e| DiskError::ImageDownload {
                asset: partial
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                message: format!(
                    "cannot unpack the payload (the release's compressed stream looks truncated): {e}"
                ),
            })?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        hasher.update(chunk);
        std::io::Write::write_all(&mut out, chunk).map_err(|source| DiskError::Io {
            path: partial.to_path_buf(),
            source,
        })?;
        written += read as u64;
        if written - last_report >= PROGRESS_STEP_BYTES {
            last_report = written;
            progress.emit(FetchProgress {
                phase: FetchPhase::Extracting,
                asset: partial
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                asset_index: 0,
                asset_count: 0,
                downloaded_bytes: written,
                total_bytes: uncompressed_total,
            });
        }
    }

    std::io::Write::flush(&mut out).map_err(|source| DiskError::Io {
        path: partial.to_path_buf(),
        source,
    })?;
    let file = out.into_inner().map_err(|e| DiskError::Io {
        path: partial.to_path_buf(),
        source: e.into_error(),
    })?;
    file.sync_all().map_err(|source| DiskError::Io {
        path: partial.to_path_buf(),
        source,
    })?;

    if let Some(expected) = expected_sha256 {
        let actual = hex(&hasher.finalize());
        if actual != expected {
            return Err(DiskError::ImageVerify {
                message: format!(
                    "unpacked image does not match its manifest (expected sha256 {expected}, got {actual}); \
                     the download was corrupted or the release is inconsistent"
                ),
            });
        }
    }

    progress.emit(FetchProgress {
        phase: FetchPhase::Extracting,
        asset: partial
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        asset_index: 0,
        asset_count: 0,
        downloaded_bytes: written,
        total_bytes: uncompressed_total.max(written),
    });

    Ok(())
}

async fn download_asset(
    client: &reqwest::Client,
    asset: &RemoteAsset,
    dest: &Path,
    expected_sha256: Option<&str>,
    already_downloaded: u64,
    total: u64,
    progress: &ProgressSink,
) -> Result<(), DiskError> {
    let mut last_error = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match download_asset_once(
            client,
            asset,
            dest,
            expected_sha256,
            already_downloaded,
            total,
            progress,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_error = e.to_string();
                tracing::warn!(
                    asset = %asset.name,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    error = %last_error,
                    "asset download attempt failed"
                );
                let _ = std::fs::remove_file(dest);
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
                }
            }
        }
    }
    Err(DiskError::ImageDownload {
        asset: asset.name.clone(),
        message: format!("{last_error} (gave up after {MAX_ATTEMPTS} attempts)"),
    })
}

async fn download_asset_once(
    client: &reqwest::Client,
    asset: &RemoteAsset,
    dest: &Path,
    expected_sha256: Option<&str>,
    already_downloaded: u64,
    total: u64,
    progress: &ProgressSink,
) -> Result<(), DiskError> {
    use sha2::Digest;

    let mut response = client
        .get(&asset.url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .map_err(|e| DiskError::ImageDownload {
            asset: asset.name.clone(),
            message: format!("request failed: {e}"),
        })?;

    if !response.status().is_success() {
        return Err(DiskError::ImageDownload {
            asset: asset.name.clone(),
            message: format!("HTTP {}", response.status()),
        });
    }

    let file = std::fs::File::create(dest).map_err(|source| DiskError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    let mut out = std::io::BufWriter::with_capacity(4 * 1024 * 1024, file);
    let mut hasher = sha2::Sha256::new();
    let mut written: u64 = 0;
    let mut last_report: u64 = 0;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| DiskError::ImageDownload {
            asset: asset.name.clone(),
            message: format!("transfer interrupted: {e}"),
        })?
    {
        hasher.update(&chunk);
        std::io::Write::write_all(&mut out, &chunk).map_err(|source| DiskError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
        written += chunk.len() as u64;
        if written - last_report >= PROGRESS_STEP_BYTES {
            last_report = written;
            progress.emit(FetchProgress {
                phase: FetchPhase::Downloading,
                asset: asset.name.clone(),
                asset_index: 0,
                asset_count: 0,
                downloaded_bytes: already_downloaded + written,
                total_bytes: total,
            });
        }
    }

    std::io::Write::flush(&mut out).map_err(|source| DiskError::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    if asset.size_bytes > 0 && written != asset.size_bytes {
        return Err(DiskError::ImageDownload {
            asset: asset.name.clone(),
            message: format!(
                "got {written} bytes, the release lists {} bytes",
                asset.size_bytes
            ),
        });
    }

    if let Some(expected) = expected_sha256 {
        let actual = hex(&hasher.finalize());
        if actual != expected {
            return Err(DiskError::ImageVerify {
                message: format!(
                    "asset `{}` does not match its manifest sha256 (expected {expected}, got {actual})",
                    asset.name
                ),
            });
        }
    }

    progress.emit(FetchProgress {
        phase: FetchPhase::Downloading,
        asset: asset.name.clone(),
        asset_index: 0,
        asset_count: 0,
        downloaded_bytes: already_downloaded + written,
        total_bytes: total,
    });

    Ok(())
}

async fn get_bytes(client: &reqwest::Client, url: &str, what: &str) -> Result<Vec<u8>, DiskError> {
    let mut last_error = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let result = client
            .get(url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await;
        let outcome = match result {
            Ok(response) if response.status().is_success() => match response.bytes().await {
                Ok(body) => return Ok(body.to_vec()),
                Err(e) => format!("cannot read the response body: {e}"),
            },
            Ok(response) => format!("HTTP {}", response.status()),
            Err(e) => e.to_string(),
        };
        last_error = outcome;
        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
        }
    }

    Err(DiskError::ImageIndex {
        url: url.to_string(),
        message: format!("{what}: {last_error}"),
    })
}

/// Reads a list of files as one continuous stream — the parts of one
/// compressed payload are byte ranges of a single stream, so concatenation
/// is exactly what a decompressor must see.
struct ChainedReader {
    files: std::vec::IntoIter<std::fs::File>,
    current: Option<std::fs::File>,
}

impl ChainedReader {
    fn new(files: Vec<std::fs::File>) -> Self {
        let mut files = files.into_iter();
        let current = files.next();
        Self { files, current }
    }
}

impl Read for ChainedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let Some(file) = self.current.as_mut() else {
                return Ok(0);
            };
            let read = file.read(buf)?;
            if read == 0 {
                self.current = self.files.next();
                continue;
            }
            return Ok(read);
        }
    }
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn sha256_of_file(path: &Path) -> Result<String, DiskError> {
    use sha2::Digest;
    let file = std::fs::File::open(path).map_err(|source| DiskError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buf).map_err(|source| DiskError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that swap process-wide env vars (ANDLER_HOME).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn source_trims_trailing_slashes_and_reads_env() {
        let source = ImageSource::new("acme/andler", "http://127.0.0.1:9/");
        assert_eq!(source.api_base, "http://127.0.0.1:9");
        assert_eq!(
            source.releases_url(),
            "http://127.0.0.1:9/repos/acme/andler/releases"
        );
        assert_eq!(source.describe(), "acme/andler (api http://127.0.0.1:9)");
    }

    #[test]
    fn filter_matches_variant_case_insensitively() {
        let manifest = base_image::parse_manifest(
            br#"{"android_major":"13","android_variant":"VANILLA","built_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        assert!(ImageFilter {
            android_major: Some("13".into()),
            android_variant: Some("vanilla".into()),
        }
        .matches(&manifest));
        assert!(!ImageFilter {
            android_major: Some("11".into()),
            android_variant: None,
        }
        .matches(&manifest));
    }

    #[test]
    fn chained_reader_concatenates_parts_in_order() {
        let dir = std::env::temp_dir().join(format!("andler-chain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("a");
        let second = dir.join("b");
        std::fs::write(&first, b"abc").unwrap();
        std::fs::write(&second, b"def").unwrap();

        let files = vec![
            std::fs::File::open(&first).unwrap(),
            std::fs::File::open(&second).unwrap(),
        ];
        let mut reader = ChainedReader::new(files);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();

        assert_eq!(out, b"abcdef");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_source_falls_back_to_project_defaults() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(REPO_ENV);
        std::env::remove_var(API_BASE_ENV);

        let source = ImageSource::from_env();

        assert_eq!(source.repo, DEFAULT_REPO);
        assert_eq!(source.api_base, DEFAULT_API_BASE);
    }

    #[test]
    fn env_source_picks_up_overrides() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(REPO_ENV, "acme/mirror");
        std::env::set_var(API_BASE_ENV, "http://127.0.0.1:1");

        let source = ImageSource::from_env();

        std::env::remove_var(REPO_ENV);
        std::env::remove_var(API_BASE_ENV);
        assert_eq!(source.repo, "acme/mirror");
        assert_eq!(source.api_base, "http://127.0.0.1:1");
    }

    // --- published-release fixture -----------------------------------------
    //
    // The tests below drive the real HTTP path (local listener, real bytes,
    // real zstd parts) against a fixture shaped exactly like the release
    // pipeline's output: one `<stem>.manifest.json` asset plus
    // `<stem>.qcow2.zst.NN.part` assets split out of one compressed stream.

    const STEM: &str = "linux-waydroid-android13-vanilla-abc1234";
    const BUILT_AT: &str = "2026-09-01T00:00:00Z";

    struct Fixture {
        base: String,
        requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        shutdown: tokio::sync::oneshot::Sender<()>,
    }

    impl Fixture {
        /// `build` receives the bound base URL so asset routes can point at
        /// this server; every request is recorded.
        async fn start(build: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let routes = build(&format!("http://{addr}"));
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let (shutdown, mut rx) = tokio::sync::oneshot::channel::<()>();
            let recorded = requests.clone();
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        _ = &mut rx => break,
                        accepted = listener.accept() => accepted,
                    };
                    let Ok((mut socket, _)) = accepted else { break };
                    let routes = routes.clone();
                    let recorded = recorded.clone();
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut buf = vec![0u8; 8192];
                        let read = socket.read(&mut buf).await.unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..read]).to_string();
                        let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                        recorded
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(path.clone());
                        let route_path = path.split('?').next().unwrap_or(&path).to_string();
                        let body = routes
                            .iter()
                            .find(|(route, _)| route == &route_path)
                            .map(|(_, body)| body.clone());
                        let head = match &body {
                            Some(body) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                                body.len()
                            ),
                            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                        };
                        let _ = socket.write_all(head.as_bytes()).await;
                        if let Some(body) = body {
                            let _ = socket.write_all(&body).await;
                        }
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self {
                base: format!("http://{addr}"),
                requests,
                shutdown,
            }
        }

        /// Payload-part requests only: `list_remote` fetches the manifest
        /// itself, and that GET is not what these assertions are about.
        fn requested_parts(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|path| path.starts_with("/assets/") && !path.ends_with(".manifest.json"))
                .cloned()
                .collect()
        }

        fn source(&self, repo: &str) -> ImageSource {
            ImageSource::new(repo, &self.base)
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        hex(&hasher.finalize())
    }

    /// A ~300 KiB stand-in image compressed into two arbitrary byte-split
    /// parts, exactly like the release workflow produces them.
    struct ReleaseFixture {
        payload: Vec<u8>,
        parts: Vec<(String, Vec<u8>)>,
        manifest: Vec<u8>,
        manifest_asset: String,
    }

    impl ReleaseFixture {
        fn new(payload: Vec<u8>) -> Self {
            let manifest_asset = format!("{STEM}.manifest.json");
            let compressed = zstd::encode_all(std::io::Cursor::new(&payload), 3).unwrap();
            let split = compressed.len() / 2;
            let parts = vec![
                (format!("{STEM}.qcow2.zst.00"), compressed[..split].to_vec()),
                (format!("{STEM}.qcow2.zst.01"), compressed[split..].to_vec()),
            ];
            let parts_json: Vec<String> = parts
                .iter()
                .map(|(name, bytes)| {
                    format!(
                        r#"{{"name":"{name}","sha256":"{}","size_bytes":{}}}"#,
                        sha256_hex(bytes),
                        bytes.len()
                    )
                })
                .collect();
            let manifest = format!(
                r#"{{"schema_version":1,"source_image":"andler-base:android13-vanilla","built_at":"{BUILT_AT}","git_rev":"abc1234","file_size_bytes":{},"sha256":"{}","android_major":"13","android_variant":"VANILLA","compression":"zstd","parts":[{}]}}"#,
                payload.len(),
                sha256_hex(&payload),
                parts_json.join(",")
            );
            Self {
                payload,
                parts,
                manifest: manifest.into_bytes(),
                manifest_asset,
            }
        }

        fn asset_url(base: &str, name: &str) -> String {
            format!("{base}/assets/{name}")
        }

        fn releases_json_without_parts(&self, base: &str) -> Vec<u8> {
            format!(
                r#"[{{"tag_name":"base-image-android13-vanilla-20260901-000000","draft":false,"prerelease":false,"assets":[{{"name":"{}","browser_download_url":"{}","size":{}}}]}}]"#,
                self.manifest_asset,
                Self::asset_url(base, &self.manifest_asset),
                self.manifest.len()
            )
            .into_bytes()
        }

        fn releases_json(&self, base: &str) -> Vec<u8> {
            let parts_json: Vec<String> = self
                .parts
                .iter()
                .map(|(name, bytes)| {
                    format!(
                        r#"{{"name":"{name}","browser_download_url":"{}","size":{}}}"#,
                        Self::asset_url(base, name),
                        bytes.len()
                    )
                })
                .collect();
            format!(
                r#"[{{"tag_name":"base-image-android13-vanilla-20260901-000000","draft":false,"prerelease":false,"assets":[{{"name":"{}","browser_download_url":"{}","size":{}}},{}]}}]"#,
                self.manifest_asset,
                Self::asset_url(base, &self.manifest_asset),
                self.manifest.len(),
                parts_json.join(",")
            )
            .into_bytes()
        }

        fn routes(&self, base: &str) -> Vec<(String, Vec<u8>)> {
            let mut routes = vec![
                (
                    "/repos/acme/andler/releases".to_string(),
                    self.releases_json(base),
                ),
                (
                    format!("/assets/{}", self.manifest_asset),
                    self.manifest.clone(),
                ),
            ];
            for (name, bytes) in &self.parts {
                routes.push((format!("/assets/{name}"), bytes.clone()));
            }
            routes
        }
    }

    fn image_payload() -> Vec<u8> {
        let mut payload = Vec::with_capacity(300 * 1024);
        let mut state: u32 = 0x1234_5678;
        while payload.len() < 300 * 1024 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            payload.extend_from_slice(&state.to_le_bytes());
        }
        payload
    }

    struct CacheGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        base: PathBuf,
    }

    impl CacheGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let base = std::env::temp_dir().join(format!(
                "andler-image-download-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(base.join("cache/base-images")).unwrap();
            std::env::set_var(andler_core::paths::ANDLER_HOME_ENV, &base);
            Self { _lock: lock, base }
        }
    }

    impl Drop for CacheGuard {
        fn drop(&mut self) {
            std::env::remove_var(andler_core::paths::ANDLER_HOME_ENV);
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn collect_progress() -> (
        ProgressSink,
        std::sync::Arc<std::sync::Mutex<Vec<FetchProgress>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let sink = ProgressSink::new(move |progress| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(progress);
        });
        (sink, seen)
    }

    #[tokio::test]
    async fn list_remote_reads_the_published_catalog() {
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");

        let images = list_remote(&source, &ImageFilter::default()).await.unwrap();

        assert_eq!(images.len(), 1);
        let image = &images[0];
        assert_eq!(
            image.release_tag,
            "base-image-android13-vanilla-20260901-000000"
        );
        assert_eq!(image.stem, STEM);
        assert_eq!(image.id(), format!("android13-vanilla-{BUILT_AT}"));
        assert_eq!(image.android_variant(), "VANILLA");
        assert_eq!(image.asset_count(), 2);
        assert_eq!(
            image.download_bytes(),
            fixture
                .parts
                .iter()
                .map(|(_, b)| b.len() as u64)
                .sum::<u64>()
        );
        assert_eq!(image.installed_bytes(), Some(fixture.payload.len() as u64));
        assert_eq!(
            image.qcow2_sha256(),
            Some(sha256_hex(&fixture.payload).as_str())
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn list_remote_filters_by_version_and_variant() {
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");

        let matching = list_remote(
            &source,
            &ImageFilter {
                android_major: Some("13".into()),
                android_variant: Some("gapps".into()),
            },
        )
        .await
        .unwrap();
        let other_major = list_remote(
            &source,
            &ImageFilter {
                android_major: Some("11".into()),
                android_variant: None,
            },
        )
        .await
        .unwrap();

        assert!(matching.is_empty(), "GAPPS filter must not match VANILLA");
        assert!(
            other_major.is_empty(),
            "Android 11 filter must not match 13"
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn list_remote_skips_a_release_with_a_missing_part_asset() {
        let fixture = ReleaseFixture::new(image_payload());
        // The release advertises a manifest that lists parts the release does
        // not carry: an incomplete publication must not be offered as
        // installable (and must not fail the whole listing).
        let server = Fixture::start(|base| {
            vec![
                (
                    "/repos/acme/andler/releases".to_string(),
                    fixture.releases_json_without_parts(base),
                ),
                (
                    format!("/assets/{}", fixture.manifest_asset),
                    fixture.manifest.clone(),
                ),
            ]
        })
        .await;
        let source = server.source("acme/andler");

        let images = list_remote(&source, &ImageFilter::default()).await.unwrap();

        assert!(images.is_empty(), "a release missing its parts is skipped");
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_installs_a_verified_image_into_the_cache() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);
        let (sink, progress) = collect_progress();

        let outcome = fetch_base_image(&source, &image, false, &sink)
            .await
            .unwrap();

        assert!(!outcome.reused);
        assert_eq!(
            std::fs::read(&outcome.qcow2_path).unwrap(),
            fixture.payload,
            "the installed image must be the decompressed payload"
        );
        let manifest = andler_core::base_image::read_manifest(&outcome.manifest_path).unwrap();
        assert_eq!(manifest.id(), image.id());
        assert_eq!(manifest.android_variant, "VANILLA");
        assert!(
            outcome
                .qcow2_path
                .parent()
                .unwrap()
                .ends_with("android13-vanilla"),
            "images land in the per-version-variant cache subdirectory: {:?}",
            outcome.qcow2_path
        );
        assert_eq!(
            base_image::info_for(&outcome.qcow2_path).map(|info| info.id()),
            Some(image.id()),
            "the installed image must be discoverable by the daemon"
        );
        assert!(
            installed_image(&image.id()).is_some(),
            "the installed build must be reported as cached"
        );
        assert!(image.is_installed());
        assert!(
            !outcome
                .qcow2_path
                .parent()
                .unwrap()
                .join(format!(".fetch-{STEM}"))
                .exists(),
            "the download scratch directory must be removed after installing"
        );

        let phases: Vec<FetchPhase> = progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|event| event.phase)
            .collect();
        for expected in [
            FetchPhase::Downloading,
            FetchPhase::Verifying,
            FetchPhase::Extracting,
            FetchPhase::Installing,
        ] {
            assert!(
                phases.contains(&expected),
                "progress must report {expected:?}, saw {phases:?}"
            );
        }
        let bytes_reported = progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|event| event.phase == FetchPhase::Downloading)
            .map(|event| event.downloaded_bytes)
            .max()
            .unwrap();
        assert_eq!(
            bytes_reported,
            image.download_bytes(),
            "download progress must reach the published payload size"
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_reuses_an_already_installed_build_without_touching_the_network() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);
        let first = fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap();
        let requests_before = server.requested_parts().len();

        let second = fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap();

        assert!(second.reused, "a cached build must not be downloaded again");
        assert_eq!(second.qcow2_path, first.qcow2_path);
        assert_eq!(
            server.requested_parts().len(),
            requests_before,
            "the reuse path must not fetch assets"
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_reuses_verified_parts_after_an_interrupted_download() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);

        // Leave the first part behind, as a killed download would.
        let staging = andler_core::paths::base_images_dir()
            .join("android13-vanilla")
            .join(format!(".fetch-{STEM}"));
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join(&fixture.parts[0].0), &fixture.parts[0].1).unwrap();

        let outcome = fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap();

        assert!(!outcome.reused);
        assert_eq!(std::fs::read(&outcome.qcow2_path).unwrap(), fixture.payload);
        assert_eq!(
            server.requested_parts(),
            vec![format!("/assets/{}", fixture.parts[1].0)],
            "the already-verified part must not be downloaded twice"
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_refuses_a_part_that_does_not_match_its_manifest() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        // Corrupt part 00 on the wire: its bytes no longer hash to the
        // sha256 the manifest declares.
        let server = Fixture::start(|base| {
            fixture
                .routes(base)
                .into_iter()
                .map(|(path, mut body)| {
                    if path.ends_with(&fixture.parts[0].0) {
                        body[0] ^= 0xff;
                    }
                    (path, body)
                })
                .collect()
        })
        .await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);

        let err = fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("does not match its manifest sha256"),
            "corruption must be reported as a verification failure: {message}"
        );
        assert!(
            !image.install_path().exists(),
            "nothing may be installed from a failed download"
        );
        assert!(
            !image
                .install_path()
                .parent()
                .unwrap()
                .join(format!(".fetch-{STEM}"))
                .exists(),
            "failed downloads must not leave scratch files behind"
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_refuses_a_payload_that_does_not_match_the_manifest_sha256() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        // The parts are intact but the final qcow2 hash in the manifest is
        // wrong (e.g. the release published a stale manifest).
        let stale_manifest = String::from_utf8(fixture.manifest.clone())
            .unwrap()
            .replace(&sha256_hex(&fixture.payload), &"0".repeat(64))
            .into_bytes();
        let server = Fixture::start(|base| {
            fixture
                .routes(base)
                .into_iter()
                .map(|(path, body)| {
                    if path == format!("/assets/{}", fixture.manifest_asset) {
                        (path, stale_manifest.clone())
                    } else {
                        (path, body)
                    }
                })
                .collect()
        })
        .await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);

        let err = fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("unpacked image does not match"),
            "a stale manifest hash must fail the install: {err}"
        );
        assert!(!image.install_path().exists());
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_replaces_a_cached_build_when_forced() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);
        fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap();
        let requests_before = server.requested_parts().len();

        let outcome = fetch_base_image(&source, &image, true, &ProgressSink::silent())
            .await
            .unwrap();

        assert!(
            !outcome.reused,
            "--force must re-download an installed build"
        );
        assert_eq!(
            server.requested_parts().len(),
            requests_before + 2,
            "both parts must be fetched again"
        );
        let _ = server.shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_reports_an_unreachable_host_and_installs_nothing() {
        let _cache = CacheGuard::new();
        let fixture = ReleaseFixture::new(image_payload());
        let server = Fixture::start(|base| fixture.routes(base)).await;
        let source = server.source("acme/andler");
        let image = list_remote(&source, &ImageFilter::default())
            .await
            .unwrap()
            .remove(0);
        // The listing succeeded, so its asset URLs point at this server —
        // which is now gone, as a dropped connection mid-download would be.
        let _ = server.shutdown.send(());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let err = fetch_base_image(&source, &image, false, &ProgressSink::silent())
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("cannot download base-image asset")
                && message.contains(&fixture.parts[0].0),
            "an unreachable host must name the failing asset: {message}"
        );
        assert!(!image.install_path().exists());
    }
}
