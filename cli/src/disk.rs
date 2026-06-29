use crate::helpers::{format_size, parse_size};
use crate::DiskAction;

pub async fn handle(action: DiskAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DiskAction::Create { path, size } => {
            let bytes = parse_size(&size)?;
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
        DiskAction::Resize { path, size } => {
            let bytes = parse_size(&size)?;
            andler_disk::qcow2::resize(&path, bytes).await?;
            println!("resized {} to {}", path.display(), format_size(bytes));
        }
        DiskAction::Compact { path } => {
            andler_disk::qcow2::compact(&path).await?;
            println!("compacted {}", path.display());
        }
    }
    Ok(())
}
