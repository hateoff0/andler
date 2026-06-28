# scripts

Helper development scripts, not part of the product.

## `start.sh`

Reference QEMU launch script with 3D acceleration via Venus. This is the source of truth for `backends/andler-qemu/src/cmdline.rs` — any discrepancy between what `cmdline.rs` generates and this script should be investigated as a potential regression.
