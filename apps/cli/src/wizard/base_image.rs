use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, BaseImageDownloadPhase, DownloadBaseImageRequest,
    ListRemoteBaseImagesRequest, RemoteBaseImageEntry,
};

use crate::{CliAndroidVersion, TracedClient};

use super::basic::validate_base_image_path;
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

/// The two answers to "where does the image come from", as values: the labels
/// carry a build id and a size, which is not something to parse back out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Published,
    Manual,
}

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
        None => fetch_catalog(client, version, gapps).await?,
    };

    let choice = match &published {
        Some(image) => cliclack::select(format!(
            "No {variant} image for Android {} in the cache\nwhere should the guest image come from?",
            version.number()
        ))
        .item(
            Source::Published,
            "Download the published build",
            // The build id is 36 columns long and the list prefix takes five:
            // the id belongs in the confirmation question below (and in the
            // summary), not on the highlighted line.
            format!("{} — into the cache", super::basic::describe_size(image.download_bytes)),
        )
        .item(
            Source::Manual,
            "Enter a path manually",
            "an existing .qcow2 on this host",
        )
        .initial_value(Source::Published)
        .interact()?,
        None => {
            // One option is not a question: the catalog said there is no
            // build to offer, so say why and ask for the path directly.
            ui::info(format!(
                "No {variant} image for Android {} in the cache, and no published build was \
                 found. Build one with `docker/images/build.sh <11|13> <VANILLA|GAPPS>`, \
                 download a published one with `andler image download`, or point at an \
                 existing .qcow2.",
                version.number()
            ))?;
            Source::Manual
        }
    };

    if let (Source::Published, Some(image)) = (choice, published) {
        return download(client, &image).await;
    }

    let path: String = cliclack::input("Path to the Android base image")
        .placeholder("/home/user/.andler/cache/base-images/android13-vanilla/…")
        .validate(|input: &String| {
            validate_base_image_path(input.trim()).map_err(|e| e.to_string())
        })
        .interact()?;

    Ok(Choice {
        path: path.trim().to_string(),
        auto_resolved: false,
    })
}

/// The newest published build for this (version, package set), if the daemon
/// can reach the release index. A wizard question must never fail because the
/// catalog is unreachable — it falls back to the manual path with a note.
async fn fetch_catalog(
    client: &mut TracedClient,
    version: CliAndroidVersion,
    gapps: bool,
) -> Result<Option<RemoteBaseImageEntry>, WizardError> {
    let spinner = cliclack::spinner();
    spinner.start("reading the published base-image catalog");
    let result = client
        .list_remote_base_images(ListRemoteBaseImagesRequest {
            android_version: ProtoAndroidVersion::from(version) as i32,
            android_variant: if gapps { "GAPPS" } else { "VANILLA" }.to_string(),
        })
        .await;

    match result {
        Ok(response) => {
            let found = response.into_inner().images.into_iter().next();
            match &found {
                Some(image) => spinner.stop(format!("published build: {}", image.id)),
                None => spinner.stop("the pipeline has published no build for this configuration"),
            }
            Ok(found)
        }
        Err(e) => {
            // The daemon's message is long and carries the actionable half
            // (which env var to point elsewhere), so it goes through the log
            // line — written after the spinner is stopped — instead of the
            // transient progress line, whose tail the next redraw overwrites.
            spinner.error("the published base-image catalog could not be read");
            ui::warn(crate::format_grpc_error(&e))?;
            Ok(None)
        }
    }
}

async fn download(
    client: &mut TracedClient,
    image: &RemoteBaseImageEntry,
) -> Result<Choice, WizardError> {
    let confirmed = cliclack::confirm(format!(
        "Download {} ({})?\nverified against the release manifest's sha256\n\
         installed into ~/.andler/cache/base-images/",
        image.id,
        super::basic::describe_size(image.download_bytes)
    ))
    .initial_value(true)
    .interact()?;
    if !confirmed {
        return Err(WizardError::Cancelled);
    }

    // The same bar `andler image download` reports through: one bar for the
    // whole payload, moving only forward — across parts, and across the
    // extraction that follows them.
    let progress = crate::helpers::download_bar();
    progress.start("downloading the base image");

    let mut stream = match client
        .download_base_image(DownloadBaseImageRequest {
            android_version: ProtoAndroidVersion::Unspecified as i32,
            android_variant: String::new(),
            release_tag: image.release_tag.clone(),
            force: false,
        })
        .await
    {
        Ok(response) => response.into_inner(),
        Err(e) => {
            // The daemon's message is long and names what to do next, so it
            // goes through the log line (which wraps) rather than the one-line
            // progress frame (which does not).
            let message = crate::format_grpc_error(&e);
            progress.error("the download could not be started");
            ui::warn(&message)?;
            return Err(WizardError::Message(message));
        }
    };

    let mut installed = String::new();
    loop {
        match stream.message().await {
            Ok(Some(message)) => {
                crate::helpers::update_download_bar(&progress, &message);
                if message.phase() == BaseImageDownloadPhase::Done {
                    installed = message.installed_path;
                }
            }
            Ok(None) => break,
            Err(e) => {
                let message = crate::format_grpc_error(&e);
                progress.error("the download failed");
                ui::warn(&message)?;
                return Err(WizardError::Message(message));
            }
        }
    }

    if installed.is_empty() {
        progress.error("the download reported no installed path");
        return Err(WizardError::Message(
            "the download finished without reporting an installed path; \
             run `andler image list` to see what is in the cache"
                .to_string(),
        ));
    }
    progress.stop("base image ready");
    // The wizard picked this build out of the published catalog, so it is as
    // "auto" as a local cache hit: if a later answer changes which image the
    // profile matches (the Android group's GApps toggle), re-resolution runs
    // and warns instead of leaving a GAPPS profile on a VANILLA image without
    // saying anything.
    Ok(Choice {
        path: installed,
        auto_resolved: true,
    })
}
