# ANDLER Comprehensive Documentation

This is the comprehensive documentation for ANDLER (Android Linux Emulator & Runtime), an open-source platform for managing virtual machines with GPU acceleration.

## Table of Contents

- [Project Overview](#project-overview)
- [Architecture](#architecture)
- [Getting Started](#getting-started)
- [Development Guide](#development-guide)
- [API Reference](#api-reference)
- [Contributing](#contributing)
- [FAQ](#faq)

## Project Overview

ANDLER is an open-source platform that provides a unified interface for managing virtual machines and Android environments on Linux systems. It's designed to work seamlessly with NVIDIA, AMD, and Intel GPUs, offering GPU acceleration through Venus or VirtIO-GPU.

### Key Features

- **Multi-GPU Support**: Works with NVIDIA, AMD, Intel, and CPU-only systems
- **Android & Linux Support**: Unified management for both Android and Linux VMs
- **GPU Acceleration**: Venus (Vulkan proxy) and VirtIO-GPU support
- **Resource Management**: Dynamic CPU/RAM allocation, disk compression, memory ballooning
- **Headless Mode**: Server-side operation without GUI
- **Multi-Instance**: Run multiple VMs simultaneously with shared resources

## Architecture

ANDLER follows a modular architecture with clear separation of concerns:

```
┌─────────────────────────────────────────────────────────────┐
│                        ANDLER Platform                      │
├─────────────────────────────────────────────────────────────┤
│  Frontend (Tauri)  │  CLI (andler)  │  Daemon (andlerd)    │
├─────────────────────────────────────────────────────────────┤
│  gRPC API Layer (andler-rpc)                              │
├─────────────────────────────────────────────────────────────┤
│  Core Logic Layer                                       │
├─────────────────────────────────────────────────────────────┤
│  Backend Implementations                                │
├─────────────────────────────────────────────────────────────┤
│  System Dependencies (QEMU, KVM, etc.)                   │
└─────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

- **Frontend**: GUI client for managing VMs (Tauri-based)
- **CLI**: Command-line interface for VM management
- **Daemon**: Background service managing VM lifecycle
- **Core**: Business logic and domain models
- **Backend**: VM execution implementations (QEMU, rust-vmm)
- **RPC**: gRPC API for communication between components

## Getting Started

### Prerequisites

- Linux system with KVM support (`/dev/kvm`)
- User in `kvm` group
- QEMU system packages:
  - `qemu-system-x86_64`
  - `qemu-utils`
  - `ovmf` (UEFI firmware)

### Installation

#### From Source

```bash
# Clone the repository
git clone https://github.com/andler-project/andler.git
cd andler

# Build the project
cargo build --workspace

# Run tests
cargo test --workspace
```

#### Using Docker (Recommended)

```bash
# Build unit tests (no KVM required)
docker compose run --rm unit-test

# Build integration tests (requires KVM)
docker compose run --rm integration-test

# Run daemon for development
docker compose up daemon
```

### Basic Usage

```bash
# Create a new VM instance
andler create --name my-vm --type linux --iso /path/to/iso

# Start the VM
andler start my-vm

# Check VM status
andler status my-vm

# Stop the VM
andler stop my-vm

# List all VMs
andler list
```

## Development Guide

### Project Structure

```
andler/
├── crates/
│   ├── andler-core/     # Domain logic and VM configuration
│   ├── andler-qemu/     # QEMU backend implementation
│   ├── andler-vmm/      # rust-vmm backend (future)
│   ├── andler-disk/     # Disk management (qcow2 operations)
│   ├── andler-net/      # Network configuration
│   ├── andler-store/    # SQLite persistence
│   ├── andler-rpc/      # gRPC API definition
│   ├── andler-daemon/   # Background service
│   └── andler-cli/      # Command-line interface
├── frontend/            # Tauri GUI client (future)
├── guest-image/         # Guest image building (future)
├── presets/             # VM presets (future)
├── packaging/           # Distribution packaging (future)
└── docker/              # Docker development environment
```

### Building and Testing

#### Unit Tests

```bash
# Run all unit tests (no KVM required)
cargo test --workspace

# Run tests for specific crate
cargo test -p andler-core
```

#### Integration Tests

```bash
# Run integration tests (requires KVM)
cargo test --workspace -- --ignored

# Or use Docker
docker compose run --rm integration-test
```

#### Code Quality

```bash
# Format code
cargo fmt

# Run clippy
cargo clippy --workspace

# Check for warnings
cargo clippy --workspace -- -D warnings
```

### Adding a New Backend

1. Create a new crate in `crates/`
2. Implement the `HypervisorBackend` trait from `andler-core`
3. Register the backend in `andler-daemon`
4. Add appropriate tests

Example backend implementation:

```rust
use andler_core::backend::{HypervisorBackend, BackendError, BackendHandle};
use async_trait::async_trait;

#[async_trait]
impl HypervisorBackend for MyBackend {
    fn name(&self) -> &'static str {
        "my-backend"
    }
    
    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        // Implementation here
        todo!()
    }
    
    // Implement other required methods...
}
```

## API Reference

### gRPC API

The gRPC API provides the interface between frontend, CLI, and daemon components.

#### Service Definition

```protobuf
service AndlerService {
    rpc CreateInstance(CreateInstanceRequest) returns (InstanceInfo);
    rpc StartInstance(StartInstanceRequest) returns (StatusResponse);
    rpc StopInstance(StopInstanceRequest) returns (StatusResponse);
    rpc PauseInstance(PauseInstanceRequest) returns (StatusResponse);
    rpc ResumeInstance(ResumeInstanceRequest) returns (StatusResponse);
    rpc CloneInstance(CloneInstanceRequest) returns (InstanceInfo);
    rpc RemoveInstance(RemoveInstanceRequest) returns (StatusResponse);
    rpc ListInstances(ListInstancesRequest) returns (InstanceList);
    rpc GetInstanceStatus(GetInstanceStatusRequest) returns (InstanceInfo);
    rpc StreamInstanceLogs(StreamInstanceLogsRequest) returns (stream LogEntry);
    rpc StreamResourceMetrics(StreamResourceMetricsRequest) returns (stream ResourceMetrics);
}
```

#### Request/Response Types

```protobuf
message CreateInstanceRequest {
    string name = 1;
    InstanceKind kind = 2;
    BackendKind backend = 3;
    CpuConfig cpu = 4;
    MemoryConfig memory = 5;
    DiskConfig disk = 6;
    DisplayConfig display = 7;
    RenderBackend render_backend = 8;
    NetworkConfig network = 9;
}

message InstanceInfo {
    string id = 1;
    string name = 2;
    InstanceState state = 3;
    ResourceMetrics metrics = 4;
    repeated string tags = 5;
}
```

### CLI Commands

#### VM Management

- `create` - Create a new VM instance
- `start` - Start a VM instance
- `stop` - Stop a VM instance
- `pause` - Pause a VM instance
- `resume` - Resume a paused VM instance
- `clone` - Clone an existing VM instance
- `remove` - Remove a VM instance
- `list` - List all VM instances
- `status` - Get status of a VM instance

#### Configuration

- `config` - View or modify configuration
- `snapshot` - Create or restore snapshots

### Core Types

#### InstanceConfig

```rust
pub struct InstanceConfig {
    pub id: InstanceId,
    pub name: String,
    pub kind: InstanceKind,
    pub backend: BackendKind,
    pub cpu: CpuConfig,
    pub memory: MemoryConfig,
    pub disk: DiskConfig,
    pub display: DisplayConfig,
    pub render_backend: RenderBackend,
    pub network: NetworkConfig,
}
```

#### RenderBackend

```rust
pub enum RenderBackend {
    Venus,           // Vulkan proxy for maximum performance
    VirtioGpu,       // Universal virtual GPU
    VirGl,           // OpenGL ES proxy
    Cpu,             // Software rendering
    Passthrough,     // GPU passthrough (future)
}
```

## Contributing

### Development Setup

1. **Install Rust**: Follow the official [Rust installation guide](https://www.rust-lang.org/tools/install)
2. **Install dependencies**:
   ```bash
   # For development
   sudo apt install qemu-system-x86 qemu-utils ovmf  # Debian/Ubuntu
   # or
   sudo dnf install qemu-kvm qemu-img edk2-ovmf      # Fedora
   
   # Install Rust tools
   rustup component add rustfmt clippy
   ```

3. **Clone and build**:
   ```bash
   git clone https://github.com/andler-project/andler.git
   cd andler
   cargo build --workspace
   ```

### Code Style

- Follow Rust formatting conventions (`cargo fmt`)
- Use clippy for linting (`cargo clippy`)
- Write documentation comments for public APIs
- Add tests for new functionality

### Pull Request Process

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests for new functionality
5. Run tests and linting
6. Submit a pull request

### Testing Guidelines

- **Unit Tests**: Test individual components in isolation
- **Integration Tests**: Test component interactions (requires KVM)
- **E2E Tests**: Test complete workflows (requires KVM)

## FAQ

### Common Issues

#### KVM Permission Denied

```bash
# Add user to kvm group
sudo usermod -aG kvm $USER
# Log out and back in for changes to take effect
```

#### QEMU Not Found

```bash
# Install QEMU packages
sudo apt install qemu-system-x86 qemu-utils  # Debian/Ubuntu
sudo dnf install qemu-kvm qemu-img           # Fedora
```

#### Docker Build Fails

```bash
# Clean Docker cache
docker system prune -f

# Rebuild without cache
docker compose build --no-cache
```

### Performance Tips

- Use NVMe storage for VM disks
- Enable KSM for memory deduplication
- Use Venus rendering for NVIDIA GPUs
- Allocate appropriate CPU cores (4+ recommended)
- Use SSD for better I/O performance

### Troubleshooting

#### Check VM Status

```bash
# Get detailed VM information
andler status --verbose my-vm

# Check system resources
docker compose run --rm unit-test
```

#### View Logs

```bash
# Check daemon logs
journalctl -u andlerd -f

# Check VM logs
docker compose logs daemon
```

## License

This project is licensed under the MIT License - see the [LICENSE](../LICENSE) file for details.

## Acknowledgments

- QEMU and KVM for virtualization
- Venus for Vulkan proxy support
- Waydroid for Android-in-Linux support
- The Rust community for excellent tooling
- All contributors to the project

## Contact

- **GitHub**: [https://github.com/andler-project/andler](https://github.com/andler-project/andler)
- **Issues**: [https://github.com/andler-project/andler/issues](https://github.com/andler-project/andler/issues)
- **Discussions**: [https://github.com/andler-project/andler/discussions](https://github.com/andler-project/andler/discussions)