use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, BaseImageDownloadPhase, DownloadBaseImageRequest,
    ListRemoteBaseImagesRequest, RemoteBaseImageEntry,
};
use inquire::{Confirm, Select, Text};

use crate::helpers::spinner;
use crate::{CliAndroidVersion, TracedClient};

use super::basic::validate_base_image_path;
use super::map_inquire_err;
use super::ui;
use super::WizardError;

/// Where the Android base image comes from: an existing file, or a build the
/// release pipeline published and we download into the cache.
pub struct Choice {
    pub path: String,
    /// True when the wizard picked the file itself (a local cache hit) rather
    /// than the user naming it.
    pub auto_resolved: bool,
}

const ENTER_MANUALLY: &str = "Enter a path manually";

pub async fn ask(
    client: &mut TracedClient,
    prefilled: Option<String>,
    version: CliAndroidVersion,
    gapps: bool,
) -> Result<Choice, WizardError> {
    if let Some(path) = prefilled {
        validate_base_image_path(&path)?;
        return Ok(Choice {
            path,
            auto_resolved: false,
        });
    }

    let profile = super::build::android_profile_for(version, gapps, crate::CliArmTranslator::None);
    let local = andler_core::base_image::resolve(&profile).ok();
    let variant = if gapps { "GAPPS" } else { "VANILLA" };

    let published = match local {
        Some(path) => {
            // A local match short-circuits the network: the wizard does not
            // need the catalog when the cache already has what was asked for.
            let path = path.to_string_lossy().into_owned();
            return Ok(Choice {
                path,
                auto_resolved: true,
            });
        }
        None => fetch_catalog(client, version, gapps).await,
    };

    let mut options: Vec<String> = Vec::new();
    if let Some(image) = &published {
        options.push(format!(
            "Download {} ({}) from the release pipeline",
            image.id,
            super::basic::describe_size(image.download_bytes)
        ));
    }
    options.push(ENTER_MANUALLY.to_string());

    let prompt = format!("No matching {variant} image for Android {version:?} in the cache:");
    let choice = Select::new(&prompt, options.clone())
        .with_help_message(&match &published {
            Some(image) => format!(
                "Downloads into ~/.andler/cache/base-images/ and installs the image there; \
                 published as release {}",
                image.release_tag
            ),
            None => {
                "Build one with docker/images/build.sh, or point at an existing .qcow2".to_string()
            }
        })
        .prompt()
        .map_err(map_inquire_err)?;

    match published {
        Some(image) if choice == options[0] => download(client, &image).await,
        _ => {
            ui::note(
                "Expected an existing .qcow2 built by docker/images/build.sh \
                 (a plain path also works).",
            );
            let path = Text::new("Path to Android base image:")
                .with_placeholder("/home/user/.andler/cache/base-images/android13-vanilla/…")
                .with_validator(|s: &str| {
                    if s.trim().is_empty() {
                        Ok(inquire::validator::Validation::Invalid(
                            "Base image path is required".into(),
                        ))
                    } else {
                        Ok(inquire::validator::Validation::Valid)
                    }
                })
                .prompt()
                .map_err(map_inquire_err)?;
            validate_base_image_path(&path)?;
            Ok(Choice {
                path,
                auto_resolved: false,
            })
        }
    }
}

/// The newest published build for this (version, package set), if the daemon
/// can reach the release index. A wizard question must never fail because the
/// catalog is unreachable — it falls back to the manual path with a note.
async fn fetch_catalog(
    client: &mut TracedClient,
    version: CliAndroidVersion,
    gapps: bool,
) -> Option<RemoteBaseImageEntry> {
    let progress = spinner("Checking published base images…");
    let result = client
        .list_remote_base_images(ListRemoteBaseImagesRequest {
            android_version: ProtoAndroidVersion::from(version) as i32,
            android_variant: if gapps { "GAPPS" } else { "VANILLA" }.to_string(),
        })
        .await;
    progress.finish_and_clear();

    match result {
        Ok(response) => response.into_inner().images.into_iter().next(),
        Err(e) => {
            ui::warn(&format!(
                "Could not read the published base-image catalog: {}",
                crate::format_grpc_error(&e)
            ));
            None
        }
    }
}

async fn download(
    client: &mut TracedClient,
    image: &RemoteBaseImageEntry,
) -> Result<Choice, WizardError> {
    let confirmed = Confirm::new(&format!(
        "Download {} ({})?",
        image.id,
        super::basic::describe_size(image.download_bytes)
    ))
    .with_default(true)
    .with_help_message(
        "Verified against the sha256 in the release manifest before it is installed.",
    )
    .prompt()
    .map_err(map_inquire_err)?;
    if !confirmed {
        return Err(WizardError::Cancelled);
    }

    let progress = spinner(&format!("Downloading {}…", image.id));
    let mut stream = client
        .download_base_image(DownloadBaseImageRequest {
            android_version: ProtoAndroidVersion::Unspecified as i32,
            android_variant: String::new(),
            release_tag: image.release_tag.clone(),
            force: false,
        })
        .await
        .map_err(|e| WizardError::Inquire(crate::format_grpc_error(&e)))?
        .into_inner();

    let mut installed = String::new();
    loop {
        match stream.message().await {
            Ok(Some(message)) => {
                progress.set_message(match message.phase() {
                    BaseImageDownloadPhase::Downloading => format!(
                        "Downloading {} ({}/{})",
                        message.asset, message.asset_index, message.asset_count
                    ),
                    BaseImageDownloadPhase::Verifying => {
                        format!("Verifying {}", message.asset)
                    }
                    BaseImageDownloadPhase::Extracting => "Unpacking the image…".to_string(),
                    BaseImageDownloadPhase::Installing => "Installing into the cache…".to_string(),
                    BaseImageDownloadPhase::Resolving => "Resolving the build…".to_string(),
                    BaseImageDownloadPhase::Done => "Done".to_string(),
                    BaseImageDownloadPhase::Unspecified => String::new(),
                });
                if message.phase() == BaseImageDownloadPhase::Done {
                    installed = message.installed_path;
                }
            }
            Ok(None) => break,
            Err(e) => {
                progress.finish_and_clear();
                return Err(WizardError::Inquire(crate::format_grpc_error(&e)));
            }
        }
    }
    progress.finish_and_clear();

    if installed.is_empty() {
        return Err(WizardError::Inquire(
            "the download finished without reporting an installed path; \
             run `andler image list` to see what is in the cache"
                .to_string(),
        ));
    }
    ui::success(&format!("Base image ready: {installed}"));
    Ok(Choice {
        path: installed,
        auto_resolved: false,
    })
}
