use crate::helpers::{emit_json, format_bytes};
use crate::CliAndroidVersion;
use crate::TracedClient;
use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, BaseImageDownloadPhase, BaseImageDownloadProgress,
    DownloadBaseImageRequest, ListRemoteBaseImagesRequest, RemoteBaseImageEntry,
};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

/// Package set of a published base image; `docker/images/build.sh` builds
/// exactly these two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CliAndroidVariant {
    Vanilla,
    Gapps,
}

impl CliAndroidVariant {
    fn as_str(&self) -> &'static str {
        match self {
            CliAndroidVariant::Vanilla => "VANILLA",
            CliAndroidVariant::Gapps => "GAPPS",
        }
    }
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum ImageAction {
    /// List the base images published by the project's release pipeline
    List {
        #[arg(long, value_enum, help = "Only this Android version")]
        android_version: Option<CliAndroidVersion>,

        #[arg(long, value_enum, help = "Only this package set")]
        variant: Option<CliAndroidVariant>,
    },

    /// Download a published base image into ~/.andler/cache/base-images
    Download {
        #[arg(
            long,
            value_enum,
            required_unless_present = "release_tag",
            help = "Android version of the build to download"
        )]
        android_version: Option<CliAndroidVersion>,

        #[arg(
            long,
            value_enum,
            required_unless_present = "release_tag",
            help = "Package set of the build to download"
        )]
        variant: Option<CliAndroidVariant>,

        #[arg(
            long,
            help = "Download one exact release instead of the newest matching build"
        )]
        release_tag: Option<String>,

        #[arg(long, help = "Re-download even when the same build is already cached")]
        force: bool,
    },
}

fn version_arg(version: Option<CliAndroidVersion>) -> i32 {
    match version {
        Some(version) => ProtoAndroidVersion::from(version) as i32,
        None => ProtoAndroidVersion::Unspecified as i32,
    }
}

fn variant_arg(variant: Option<CliAndroidVariant>) -> String {
    variant.map(|v| v.as_str().to_string()).unwrap_or_default()
}

pub async fn handle(
    client: &mut TracedClient,
    action: ImageAction,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ImageAction::List {
            android_version,
            variant,
        } => {
            let response = client
                .list_remote_base_images(ListRemoteBaseImagesRequest {
                    android_version: version_arg(android_version),
                    android_variant: variant_arg(variant),
                })
                .await?
                .into_inner();

            if json {
                let entries: Vec<serde_json::Value> =
                    response.images.iter().map(entry_json).collect();
                emit_json(&serde_json::json!({
                    "source": response.source,
                    "images": entries,
                }))?;
                return Ok(());
            }

            if response.images.is_empty() {
                println!(
                    "No published base images matched. The release pipeline \
                     (.github/workflows/build-base-image.yml) publishes them as GitHub releases; \
                     build one locally with docker/images/build.sh meanwhile."
                );
                return Ok(());
            }

            println!("source: {}", response.source);
            println!("{:<46} {:>10}  Status", "Build", "Download");
            println!("{}", "-".repeat(96));
            for image in &response.images {
                let status = if image.installed {
                    format!("cached: {}", image.installed_path)
                } else {
                    "not cached".to_string()
                };
                println!(
                    "{:<46} {:>10}  {}",
                    image.id,
                    format_bytes(image.download_bytes),
                    status
                );
            }
            println!();
            println!(
                "Download one with `andler image download --android-version <11|13> \
                 --variant <vanilla|gapps>`."
            );
            Ok(())
        }
        ImageAction::Download {
            android_version,
            variant,
            release_tag,
            force,
        } => {
            let request = DownloadBaseImageRequest {
                android_version: version_arg(android_version),
                android_variant: variant_arg(variant),
                release_tag: release_tag.unwrap_or_default(),
                force,
            };
            let mut stream = client.download_base_image(request).await?.into_inner();
            let progress = if json { ProgressBar::hidden() } else { bar() };
            let mut last_reported_step = 0u64;

            while let Some(message) = stream.message().await? {
                if json {
                    println!(
                        "{{\"phase\":\"{}\",\"asset\":\"{}\",\"asset_index\":{},\"asset_count\":{},\
                         \"downloaded_bytes\":{},\"total_bytes\":{},\"message\":\"{}\",\
                         \"installed_path\":\"{}\"}}",
                        phase_name(message.phase()),
                        message.asset,
                        message.asset_index,
                        message.asset_count,
                        message.downloaded_bytes,
                        message.total_bytes,
                        message.message,
                        message.installed_path,
                    );
                } else {
                    render_progress(&progress, &message, &mut last_reported_step);
                }
            }

            progress.finish_and_clear();
            Ok(())
        }
    }
}

fn entry_json(image: &RemoteBaseImageEntry) -> serde_json::Value {
    serde_json::json!({
        "id": image.id,
        "android_major": image.android_major,
        "android_variant": image.android_variant,
        "built_at": image.built_at,
        "release_tag": image.release_tag,
        "download_bytes": image.download_bytes,
        "installed_bytes": image.installed_bytes,
        "installed": image.installed,
        "installed_path": image.installed_path,
    })
}

fn phase_name(phase: BaseImageDownloadPhase) -> &'static str {
    match phase {
        BaseImageDownloadPhase::Resolving => "resolving",
        BaseImageDownloadPhase::Downloading => "downloading",
        BaseImageDownloadPhase::Verifying => "verifying",
        BaseImageDownloadPhase::Extracting => "extracting",
        BaseImageDownloadPhase::Installing => "installing",
        BaseImageDownloadPhase::Done => "done",
        BaseImageDownloadPhase::Unspecified => "unspecified",
    }
}

fn bar() -> ProgressBar {
    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let progress = ProgressBar::new(0);
    if let Ok(style) = ProgressStyle::default_bar()
        .template("{spinner} {msg} [{bar:32}] {bytes}/{total_bytes} ({eta})")
    {
        progress.set_style(style.progress_chars("=>-"));
    }
    progress.enable_steady_tick(Duration::from_millis(200));
    progress
}

fn render_progress(
    progress: &ProgressBar,
    message: &BaseImageDownloadProgress,
    last_reported_step: &mut u64,
) {
    if progress.is_hidden() {
        // Non-interactive output stays line-oriented: phase transitions and
        // the outcome, plus one line per 10% of the payload so a log or a
        // redirected terminal shows the download is alive instead of sitting
        // silent for the minutes a multi-GB image takes. The DONE message
        // carries runtime context ("already cached", "downloaded and
        // verified") that a script needs, so it is printed here too.
        match message.phase() {
            BaseImageDownloadPhase::Downloading | BaseImageDownloadPhase::Extracting => {
                let percent = message
                    .downloaded_bytes
                    .saturating_mul(100)
                    .checked_div(message.total_bytes)
                    .unwrap_or(0);
                let step = percent / 10;
                if step > *last_reported_step {
                    *last_reported_step = step;
                    println!(
                        "{} {} — {} / {} ({percent}%)",
                        phase_name(message.phase()),
                        message.asset,
                        format_bytes(message.downloaded_bytes),
                        format_bytes(message.total_bytes),
                    );
                }
            }
            BaseImageDownloadPhase::Done => {
                println!("done: {}", message.message);
                println!("installed {}", message.installed_path);
            }
            phase => println!("{} {}", phase_name(phase), message.asset),
        }
        return;
    }

    match message.phase() {
        BaseImageDownloadPhase::Resolving => {
            progress.set_message(message.message.clone());
        }
        BaseImageDownloadPhase::Downloading | BaseImageDownloadPhase::Extracting => {
            progress.set_length(message.total_bytes);
            progress.set_position(message.downloaded_bytes);
            progress.set_message(match message.asset_count {
                0 => message.asset.clone(),
                count => format!("{} ({}/{count})", message.asset, message.asset_index),
            });
        }
        BaseImageDownloadPhase::Verifying => {
            progress.set_message(format!("verifying {}", message.asset));
        }
        BaseImageDownloadPhase::Installing => {
            progress.set_message("installing into the cache");
        }
        BaseImageDownloadPhase::Done => {
            progress.finish_and_clear();
            println!("✓ {}", message.message);
            println!("  {}", message.installed_path);
        }
        BaseImageDownloadPhase::Unspecified => {}
    }
}
