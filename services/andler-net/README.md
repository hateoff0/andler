# andler-net

Network configuration for virtual machine instances. Currently a stub crate — contains only configuration structures, no real network interface setup.

## Current State

Early stage. Contains only the `NetworkConfig` / `NetworkMode` types that are part of `InstanceConfig` (defined in `andler-core::config::network`). No host-side network configuration logic, no nftables rules, no bridge setup.

The actual network configuration for QEMU instances is applied via QEMU command-line flags (`-netdev`, `-device`) in `andler-qemu::cmdline::network_args`. Bridge/Isolated modes will require host-side setup (bridge creation, nftables rules) before QEMU can use them — that logic will appear here when implemented.

## What Will Be Here

- Bridge network setup (creating/configuring Linux bridges)
- Isolated network mode (no external connectivity)
- nftables/iptables rule management
- Network namespace support (future)

## Dependencies

Depends on `andler-core` for `NetworkConfig` and `NetworkMode` types. No other workspace dependencies.
