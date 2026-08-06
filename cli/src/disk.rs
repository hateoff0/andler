use crate::helpers::{ensure_qcow2_extension, format_size, parse_size};
use crate::DiskAction;
use andler_disk::DiskError;

pub async fn handle(action: DiskAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DiskAction::Create { path, size } => {
            let bytes = parse_size(&size)?;
            if bytes == 0 {
                return Err("disk: refusing to create a 0-byte disk image".into());
            }
            let path = ensure_qcow2_extension(&path);
            andler_disk::qcow2::create(&path, bytes).await?;
            println!("created {}", path.display());
        }
        DiskAction::Info { path } => {
            let info = andler_disk::qcow2::info(&path).await?;
            println!("path:         {}", path.display());
            println!("format:       {}", info.format);
            println!("virtual_size: {}", format_size(info.virtual_size));
            let pct = if info.virtual_size > 0 {
                info.actual_size as f64 / info.virtual_size as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "actual_usage: {} ({:.1}%)",
                format_size(info.actual_size),
                pct
            );
            match info.backing_file {
                Some(bf) => println!("backing_file: {bf}"),
                None => println!("backing_file: none"),
            }
        }
        DiskAction::Resize { path, size, shrink } => {
            let bytes = parse_size(&size)?;
            match andler_disk::qcow2::resize(&path, bytes, shrink).await {
                Ok(()) => {
                    println!("resized {} to {}", path.display(), format_size(bytes));
                }
                Err(DiskError::ShrinkRequiresConfirmation {
                    path,
                    current_size_bytes,
                    requested_size_bytes,
                }) => {
                    eprintln!(
                        "refusing to shrink {} from {} to {} without confirmation",
                        path.display(),
                        format_size(current_size_bytes),
                        format_size(requested_size_bytes)
                    );
                    eprintln!(
                        "shrinking a disk is risky: the filesystem inside the guest must \
                         already be shrunk to fit, or you may lose data"
                    );
                    eprintln!("if you understand the risk, re-run with --shrink");
                    return Err("shrink not confirmed".into());
                }
                Err(other) => return Err(other.into()),
            }
        }
        DiskAction::Compact { path } => match andler_disk::qcow2::compact(&path).await {
            Ok(()) => println!("compacted {}", path.display()),
            Err(DiskError::CompactNotApplicable { path, format }) => {
                println!(
                    "compact is not applicable to `{format}` disks ({}): only qcow2 has \
                     reclaimable metadata, raw has none to compact",
                    path.display()
                );
            }
            Err(other) => return Err(other.into()),
        },
    }
    Ok(())
}
