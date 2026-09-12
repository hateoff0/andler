use std::path::PathBuf;

use andler_disk::base_image_download::{
    self, FetchOutcome, FetchProgress, ImageFilter, ImageSource, ProgressSink, RemoteImage,
};
use andler_rpc::proto;

use super::error::DaemonError;
use super::Daemon;

/// Which published build a request is after: by release tag (exact) or by
/// Android version + package set (newest matching build).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageSelector {
    pub android_major: Option<String>,
    pub android_variant: Option<String>,
    pub release_tag: Option<String>,
}

impl ImageSelector {
    pub fn filter(&self) -> ImageFilter {
        ImageFilter {
            android_major: self.android_major.clone(),
            android_variant: self.android_variant.clone(),
        }
    }

    pub fn describe(&self) -> String {
        match &self.release_tag {
            Some(tag) if !tag.is_empty() => format!("release {tag}"),
            _ => self.filter().describe(),
        }
    }

    /// The selector needs something to match on: a release tag on its own is
    /// enough, an Android version for the version-based lookup is not.
    pub fn validate(&self) -> Result<(), DaemonError> {
        if self
            .release_tag
            .as_deref()
            .is_some_and(|tag| !tag.is_empty())
        {
            return Ok(());
        }
        if self.android_major.is_some() {
            return Ok(());
        }
        Err(DaemonError::InvalidConfig(
            "downloading a base image needs either `android_version` (the newest matching \
             published build) or `release_tag` (one exact build)"
                .to_string(),
        ))
    }
}

/// One published build, with its local cache state resolved for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImageEntry {
    pub id: String,
    pub android_major: String,
    pub android_variant: String,
    pub built_at: String,
    pub release_tag: String,
    pub download_bytes: u64,
    pub installed_bytes: Option<u64>,
    pub installed: bool,
    pub installed_path: PathBuf,
}

impl RemoteImageEntry {
    fn from_remote(image: &RemoteImage) -> Self {
        let installed = base_image_download::installed_image(&image.id());
        Self {
            id: image.id(),
            android_major: image.android_major().to_string(),
            android_variant: image.android_variant().to_string(),
            built_at: image.built_at().to_string(),
            release_tag: image.release_tag.clone(),
            download_bytes: image.download_bytes(),
            installed_bytes: image.installed_bytes(),
            installed: installed.is_some(),
            installed_path: installed
                .map(|info| info.qcow2_path)
                .unwrap_or_else(|| image.install_path()),
        }
    }
}

impl Daemon {
    /// Published base images, newest build per (Android version, package
    /// set). The daemon is the only component with network access, so the
    /// CLI and any GUI both go through this.
    pub async fn list_remote_base_images(
        &self,
        selector: &ImageSelector,
    ) -> Result<(Vec<RemoteImageEntry>, String), DaemonError> {
        let source = ImageSource::from_env();
        let filter = selector.filter();
        let images = base_image_download::list_remote(&source, &filter)
            .await
            .map_err(|e| image_source_error(&source, e.to_string()))?;

        let entries = images.iter().map(RemoteImageEntry::from_remote).collect();
        Ok((entries, source.describe()))
    }

    /// Downloads the selected published build into the local base-image
    /// cache and returns where it landed. `progress` receives phase and byte
    /// updates for the duration — the RPC turns them into a stream.
    pub async fn download_base_image(
        &self,
        selector: &ImageSelector,
        force: bool,
        progress: &ProgressSink,
    ) -> Result<FetchOutcome, DaemonError> {
        selector.validate()?;
        let source = ImageSource::from_env();
        let images = base_image_download::list_remote(&source, &selector.filter())
            .await
            .map_err(|e| image_source_error(&source, e.to_string()))?;

        let image = select_image(&images, selector)?;
        base_image_download::fetch_base_image(&source, &image, force, progress)
            .await
            .map_err(DaemonError::from)
    }
}

fn select_image(
    images: &[RemoteImage],
    selector: &ImageSelector,
) -> Result<RemoteImage, DaemonError> {
    if let Some(tag) = selector
        .release_tag
        .as_deref()
        .filter(|tag| !tag.is_empty())
    {
        return images
            .iter()
            .find(|image| image.release_tag == tag)
            .cloned()
            .ok_or_else(|| {
                DaemonError::BaseImageUnavailable(format!(
                    "no published build carries release tag `{tag}`; `andler image list` shows \
                     the available ones"
                ))
            });
    }

    images
        .iter()
        .find(|image| {
            selector
                .android_major
                .as_deref()
                .is_none_or(|major| image.android_major() == major)
                && selector
                    .android_variant
                    .as_deref()
                    .is_none_or(|variant| image.android_variant().eq_ignore_ascii_case(variant))
        })
        .cloned()
        .ok_or_else(|| {
            DaemonError::BaseImageUnavailable(format!(
                "the release pipeline has not published a base image for {}: nothing in {} \
                 matches ({} published build(s) checked). Build one locally \
                 (`docker/images/build.sh <11|13> <VANILLA|GAPPS>`) or pick a published \
                 combination with `andler image list`",
                selector.describe(),
                ImageSource::from_env().describe(),
                images.len()
            ))
        })
}

/// Adds the one thing a raw network error does not say: what the operator can
/// do about it. Every failure here is either "the pipeline has not published
/// this yet" or "this host cannot reach the release index".
fn image_source_error(source: &ImageSource, message: String) -> DaemonError {
    DaemonError::BaseImageUnavailable(format!(
        "{message} — base images are published as GitHub releases by the project's CI \
         pipeline ({}); point ANDLERD_IMAGE_REPO / ANDLERD_IMAGE_API_BASE at another \
         source if you mirror them",
        source.release_page()
    ))
}

pub fn phase_to_proto(phase: base_image_download::FetchPhase) -> proto::BaseImageDownloadPhase {
    use base_image_download::FetchPhase;
    match phase {
        FetchPhase::Downloading => proto::BaseImageDownloadPhase::Downloading,
        FetchPhase::Verifying => proto::BaseImageDownloadPhase::Verifying,
        FetchPhase::Extracting => proto::BaseImageDownloadPhase::Extracting,
        FetchPhase::Installing => proto::BaseImageDownloadPhase::Installing,
    }
}

pub fn progress_to_proto(progress: &FetchProgress) -> proto::BaseImageDownloadProgress {
    proto::BaseImageDownloadProgress {
        phase: phase_to_proto(progress.phase).into(),
        asset: progress.asset.clone(),
        asset_index: progress.asset_index,
        asset_count: progress.asset_count,
        downloaded_bytes: progress.downloaded_bytes,
        total_bytes: progress.total_bytes,
        message: String::new(),
        installed_path: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(major: &str, variant: &str, built_at: &str, tag: &str) -> RemoteImage {
        let manifest = andler_core::base_image::parse_manifest(
            format!(
                r#"{{"android_major":"{major}","android_variant":"{variant}","built_at":"{built_at}"}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        RemoteImage {
            release_tag: tag.to_string(),
            stem: "linux-waydroid".to_string(),
            manifest,
            manifest_raw: Vec::new(),
            payload: base_image_download::Payload::Parts(Vec::new()),
        }
    }

    #[test]
    fn selector_needs_a_version_or_a_release_tag() {
        let empty = ImageSelector::default();
        assert!(empty.validate().is_err());

        let by_version = ImageSelector {
            android_major: Some("13".to_string()),
            ..Default::default()
        };
        assert!(by_version.validate().is_ok());

        let by_tag = ImageSelector {
            release_tag: Some("base-image-android13-vanilla-20260901".to_string()),
            ..Default::default()
        };
        assert!(by_tag.validate().is_ok());
        assert_eq!(
            by_tag.describe(),
            "release base-image-android13-vanilla-20260901"
        );
    }

    #[test]
    fn release_tag_wins_over_version_and_variant() {
        let images = vec![
            remote("13", "VANILLA", "2026-09-01T00:00:00Z", "tag-a"),
            remote("11", "VANILLA", "2026-08-01T00:00:00Z", "tag-b"),
        ];
        let selector = ImageSelector {
            android_major: Some("13".to_string()),
            release_tag: Some("tag-b".to_string()),
            ..Default::default()
        };

        let picked = select_image(&images, &selector).unwrap();

        assert_eq!(picked.release_tag, "tag-b");
    }

    #[test]
    fn version_lookup_takes_the_newest_published_build_of_that_set() {
        // list_remote already returns newest-first per (major, variant).
        let images = vec![
            remote("13", "VANILLA", "2026-09-01T00:00:00Z", "tag-new"),
            remote("13", "GAPPS", "2026-09-01T00:00:00Z", "tag-gapps"),
        ];
        let selector = ImageSelector {
            android_major: Some("13".to_string()),
            android_variant: Some("vanilla".to_string()),
            ..Default::default()
        };

        let picked = select_image(&images, &selector).unwrap();

        assert_eq!(picked.release_tag, "tag-new");
    }

    #[test]
    fn an_unpublished_combination_says_the_pipeline_has_not_published_it() {
        let images = vec![remote("11", "VANILLA", "2026-08-01T00:00:00Z", "tag-b")];
        let selector = ImageSelector {
            android_major: Some("13".to_string()),
            android_variant: Some("GAPPS".to_string()),
            ..Default::default()
        };

        let err = select_image(&images, &selector).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("has not published"), "{message}");
        assert!(message.contains("Android 13 GAPPS"), "{message}");
        assert!(message.contains("andler image list"), "{message}");
    }

    #[test]
    fn an_unknown_release_tag_lists_the_alternative() {
        let images = vec![remote("13", "VANILLA", "2026-09-01T00:00:00Z", "tag-a")];
        let selector = ImageSelector {
            release_tag: Some("missing-tag".to_string()),
            ..Default::default()
        };

        let err = select_image(&images, &selector).unwrap_err();

        assert!(err.to_string().contains("missing-tag"), "{err}");
    }

    #[test]
    fn progress_maps_every_downloader_phase() {
        use base_image_download::FetchPhase;
        let cases = [
            (
                FetchPhase::Downloading,
                proto::BaseImageDownloadPhase::Downloading,
            ),
            (
                FetchPhase::Verifying,
                proto::BaseImageDownloadPhase::Verifying,
            ),
            (
                FetchPhase::Extracting,
                proto::BaseImageDownloadPhase::Extracting,
            ),
            (
                FetchPhase::Installing,
                proto::BaseImageDownloadPhase::Installing,
            ),
        ];

        for (phase, expected) in cases {
            let message = progress_to_proto(&FetchProgress {
                phase,
                asset: "part".to_string(),
                asset_index: 1,
                asset_count: 2,
                downloaded_bytes: 10,
                total_bytes: 20,
            });
            assert_eq!(message.phase, expected as i32);
            assert_eq!(message.asset, "part");
            assert_eq!(message.downloaded_bytes, 10);
            assert_eq!(message.total_bytes, 20);
        }
    }
}
