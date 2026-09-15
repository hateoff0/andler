use andler_rpc::proto::{
    AndroidVersion as ProtoAndroidVersion, BaseImageDownloadPhase, BaseImageDownloadProgress,
    DownloadBaseImageRequest, ListRemoteBaseImagesRequest, RemoteBaseImageEntry,
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

    // One bar for the whole payload: the daemon reports bytes fetched so far
    // across every asset, so the bar only moves forward — across parts, and
    // across the extraction that follows them.
    //
    // The template is cliclack's download shape minus `[{elapsed_precise}]`
    // and with a 20-column bar: the stock one spends 98 columns on this line
    // (3 for the symbol, 25 for the message, 30 for the bar, 20 for the byte
    // counters, 4 for the ETA), and a live frame wider than the terminal wraps
    // — the redraw then leaves the wrapped remainder on screen.
    let progress = cliclack::progress_bar(1)
        .with_template("{msg} [{bar:20.cyan/blue}] {bytes}/{total_bytes} ({eta})");
    // The message stays short on purpose: it shares the line with the bar,
    // the byte counters and the ETA.
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
                if message.total_bytes > 0 {
                    progress.set_length(message.total_bytes);
                    progress.set_position(message.downloaded_bytes.min(message.total_bytes));
                }
                let line = progress_line(&message);
                if !line.is_empty() {
                    progress.set_message(line);
                }
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

/// What the line under the progress bar says for one stream message. The
/// batch position is only printed when the daemon knows it — mid-transfer
/// messages carry no asset index, and `(0/0)` is not something to show.
fn progress_line(message: &BaseImageDownloadProgress) -> String {
    match message.phase() {
        BaseImageDownloadPhase::Downloading if message.asset_count > 0 => {
            format!(
                "downloading part {}/{}",
                message.asset_index, message.asset_count
            )
        }
        BaseImageDownloadPhase::Downloading => "downloading".to_string(),
        BaseImageDownloadPhase::Verifying => "verifying".to_string(),
        BaseImageDownloadPhase::Extracting => "unpacking the image".to_string(),
        BaseImageDownloadPhase::Installing => "installing the image".to_string(),
        BaseImageDownloadPhase::Resolving => "resolving the build".to_string(),
        BaseImageDownloadPhase::Done => "done".to_string(),
        BaseImageDownloadPhase::Unspecified => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(
        phase: BaseImageDownloadPhase,
        asset: &str,
        asset_index: u32,
        asset_count: u32,
    ) -> BaseImageDownloadProgress {
        BaseImageDownloadProgress {
            phase: phase.into(),
            asset: asset.to_string(),
            asset_index,
            asset_count,
            downloaded_bytes: 0,
            total_bytes: 0,
            message: String::new(),
            installed_path: String::new(),
        }
    }

    #[test]
    fn every_phase_names_itself_on_the_progress_line() {
        assert_eq!(
            progress_line(&message(
                BaseImageDownloadPhase::Verifying,
                "image.qcow2.zst.00.part",
                1,
                2
            )),
            "verifying"
        );
        assert_eq!(
            progress_line(&message(BaseImageDownloadPhase::Extracting, "stem", 0, 0)),
            "unpacking the image"
        );
        assert_eq!(
            progress_line(&message(BaseImageDownloadPhase::Installing, "stem", 0, 0)),
            "installing the image"
        );
        assert_eq!(
            progress_line(&message(BaseImageDownloadPhase::Resolving, "stem", 0, 0)),
            "resolving the build"
        );
        assert_eq!(
            progress_line(&message(BaseImageDownloadPhase::Done, "stem", 0, 0)),
            "done"
        );
        assert_eq!(
            progress_line(&message(BaseImageDownloadPhase::Unspecified, "stem", 0, 0)),
            "",
            "an unspecified phase must not overwrite the line already on screen"
        );
    }

    #[test]
    fn the_batch_position_only_appears_when_the_daemon_knows_it() {
        assert_eq!(
            progress_line(&message(
                BaseImageDownloadPhase::Downloading,
                "image.qcow2.zst.00.part",
                1,
                2
            )),
            "downloading part 1/2"
        );
        assert_eq!(
            progress_line(&message(
                BaseImageDownloadPhase::Downloading,
                "image.qcow2.zst.00.part",
                0,
                0
            )),
            "downloading",
            "mid-transfer messages carry no index, and (0/0) is noise"
        );
    }

    #[test]
    fn a_progress_line_stays_inside_the_terminal() {
        // The download template adds `[{elapsed}] [30-char bar] {bytes}/{total}
        // ({eta})` — about 55 columns — so anything above ~24 here wraps the
        // live frame, and a wrapped frame leaves its tail behind on redraw.
        for phase in [
            BaseImageDownloadPhase::Downloading,
            BaseImageDownloadPhase::Verifying,
            BaseImageDownloadPhase::Extracting,
            BaseImageDownloadPhase::Installing,
            BaseImageDownloadPhase::Resolving,
            BaseImageDownloadPhase::Done,
        ] {
            let line = progress_line(&message(phase, "some-image.qcow2.zst.00.part", 12, 97));
            assert!(
                line.chars().count() <= 24,
                "{phase:?} renders {line:?}, which is too long for the download template's chrome"
            );
        }
    }
}
