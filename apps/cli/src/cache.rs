use andler_core::base_image::{self, GcRemoval};
use andler_core::paths::base_images_dir;
use clap::Subcommand;
use serde::Serialize;

#[derive(Clone, Subcommand)]
pub enum CacheAction {
    /// List every base image currently in the cache (all Android versions and variants)
    List,

    /// Remove superseded base-image builds and orphan files from the cache
    Clean {
        /// Report what would be removed without deleting anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Serialize)]
struct CacheRemovalEntry {
    qcow2: Option<String>,
    manifest: Option<String>,
    label: String,
}

#[derive(Serialize)]
struct CacheCleanReport {
    cache_dir: String,
    dry_run: bool,
    removed: Vec<CacheRemovalEntry>,
}

pub fn handle(action: CacheAction, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        CacheAction::List => list(json),
        CacheAction::Clean { dry_run } => clean(dry_run, json),
    }
}

fn list(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let dir = base_images_dir();
    let images = base_image::list_all()
        .map_err(|source| format!("cannot read base-image cache {}: {source}", dir.display()))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&images)?);
        return Ok(());
    }

    if images.is_empty() {
        println!("no base images found in {}", dir.display());
        return Ok(());
    }

    for image in &images {
        println!("{}  {}", image.id(), image.qcow2_path.display());
    }
    Ok(())
}

fn clean(dry_run: bool, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let dir = base_images_dir();
    let candidates = base_image::gc_candidates(&dir)
        .map_err(|source| format!("cannot read base-image cache {}: {source}", dir.display()))?;

    if candidates.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&CacheCleanReport {
                    cache_dir: dir.display().to_string(),
                    dry_run,
                    removed: Vec::new(),
                })?
            );
        } else {
            println!(
                "base-image cache is up to date (nothing superseded or orphaned in {})",
                dir.display()
            );
        }
        return Ok(());
    }

    let verb = if dry_run { "would remove" } else { "removing" };
    if json {
        let mut removed = Vec::with_capacity(candidates.len());
        for entry in &candidates {
            if !dry_run {
                remove_entry(entry)?;
            }
            removed.push(CacheRemovalEntry {
                qcow2: entry
                    .qcow2
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                manifest: entry
                    .manifest
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
                label: entry.label.clone(),
            });
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&CacheCleanReport {
                cache_dir: dir.display().to_string(),
                dry_run,
                removed,
            })?
        );
        return Ok(());
    }

    println!(
        "{verb} {} base-image build(s) in {}:",
        candidates.len(),
        dir.display()
    );
    for entry in &candidates {
        println!("  {}", entry.label);
    }

    if !dry_run {
        let mut removed = 0;
        for entry in &candidates {
            removed += remove_entry(entry)?;
        }
        println!("removed {removed} file(s).");
    }

    Ok(())
}

fn remove_entry(entry: &GcRemoval) -> Result<usize, Box<dyn std::error::Error>> {
    let mut removed = 0;
    for path in [entry.qcow2.as_deref(), entry.manifest.as_deref()] {
        let Some(path) = path else {
            continue;
        };
        match std::fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                // Already gone (another process, or a prior run); not an error.
            }
            Err(source) => {
                return Err(format!("cannot remove {}: {source}", path.display()).into());
            }
        }
    }
    Ok(removed)
}
