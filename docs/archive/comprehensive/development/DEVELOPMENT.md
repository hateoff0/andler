# Development Guide

## Getting Started

### Prerequisites

Before starting development, ensure you have the following installed:

#### System Requirements

- **Operating System**: Linux (Ubuntu 20.04+, Fedora 34+, Arch Linux)
- **Processor**: x86_64 with KVM support
- **Memory**: Minimum 4GB RAM (8GB recommended)
- **Storage**: Minimum 10GB free space (20GB recommended)
- **Network**: Internet connection for package installation

#### Required Software

1. **Rust Toolchain**
   ```bash
   # Install Rust using rustup
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   
   # Verify installation
   rustc --version
   cargo --version
   ```

2. **Development Tools**
   ```bash
   # Install Rust development tools
   rustup component add rustfmt clippy
   rustup component add rust-analyzer
   ```

3. **QEMU and KVM**
   ```bash
   # Debian/Ubuntu
   sudo apt install qemu-system-x86 qemu-utils ovmf
   
   # Fedora/RHEL
   sudo dnf install qemu-kvm qemu-img edk2-ovmf
   
   # Arch Linux
   sudo pacman -S qemu qemu-arch-extra edk2-ovmf
   ```

4. **Docker (Optional but Recommended)**
   ```bash
   # Install Docker
   curl -fsSL https://get.docker.com | sh
   
   # Add user to docker group
   sudo usermod -aG docker $USER
   
   # Verify installation
   docker --version
   ```

5. **Additional Dependencies**
   ```bash
   # Debian/Ubuntu
   sudo apt install libssl-dev libz-dev pkg-config
   
   # Fedora/RHEL
   sudo dnf install openssl-devel zlib-devel pkg-config
   
   # Arch Linux
   sudo pacman -S openssl zlib pkg-config
   ```

### Project Setup

1. **Clone the Repository**
   ```bash
   git clone https://github.com/andler-project/andler.git
   cd andler
   ```

2. **Verify Dependencies**
   ```bash
   # Check Rust installation
   rustc --version
   cargo --version
   
   # Check QEMU installation
   qemu-system-x86_64 --version
   
   # Check Docker installation (if using)
   docker --version
   ```

3. **Build the Project**
   ```bash
   # Build the project
   cargo build --workspace
   
   # Build in release mode
   cargo build --release --workspace
   ```

4. **Run Tests**
   ```bash
   # Run all tests
   cargo test --workspace
   
   # Run tests for specific crate
   cargo test -p andler-core
   
   # Run integration tests (requires KVM)
   cargo test --workspace -- --ignored
   ```

## Development Workflow

### Code Organization

```
andler/
├── crates/                    # Rust workspace
│   ├── andler-core/          # Domain logic and VM configuration
│   ├── andler-qemu/          # QEMU backend implementation
│   ├── andler-vmm/           # rust-vmm backend (future)
│   ├── andler-disk/          # Disk management (qcow2 operations)
│   ├── andler-net/           # Network configuration
│   ├── andler-store/         # SQLite persistence
│   ├── andler-rpc/           # gRPC API definition
│   ├── andler-daemon/        # Background service
│   └── andler-cli/           # Command-line interface
├── frontend/                 # Tauri GUI client (future)
├── guest-image/              # Guest image building (future)
├── presets/                  # VM presets (future)
├── packaging/                # Distribution packaging (future)
├── docker/                   # Docker development environment
└── docs/                     # Documentation
```

### Building and Testing

#### Building the Project

```bash
# Clean build
cargo clean
cargo build --workspace

# Build with optimizations
cargo build --release --workspace

# Build specific crate
cargo build -p andler-core
cargo build -p andler-qemu

# Build all tests
cargo test --workspace --all-targets
```

#### Running Tests

```bash
# Run all tests
cargo test --workspace

# Run unit tests only
cargo test --workspace --lib

# Run integration tests (requires KVM)
cargo test --workspace --test integration

# Run tests with verbose output
cargo test --workspace -- --verbose

# Run specific test
cargo test -p andler-core test_instance_config

# Run tests with specific feature
cargo test --features "debug" --workspace
```

#### Code Quality

```bash
# Format code
cargo fmt

# Run clippy
cargo clippy --workspace

# Run clippy with warnings as errors
cargo clippy --workspace -- -D warnings

# Check for formatting issues
cargo fmt --check

# Run all code quality checks
cargo fmt --check && cargo clippy --workspace -- -D warnings
```

### Development Environment

#### Using Docker

```bash
# Build unit tests (no KVM required)
docker compose run --rm unit-test

# Build integration tests (requires KVM)
docker compose run --rm integration-test

# Run daemon for development
docker compose up daemon

# Run daemon with specific version
docker compose up -d daemon --scale daemon=1
```

#### Local Development

```bash
# Run daemon in development mode
cargo run -p andler-daemon -- --dev

# Run CLI in development mode
cargo run -p andler-cli -- --dev

# Run with debug logging
RUST_LOG=debug cargo run -p andler-daemon
```

### Debugging

#### Debugging with GDB

```bash
# Build with debug symbols
cargo build --debug

# Run with GDB
gdb --args target/debug/andlerd

# Run with LLDB
lldb -- target/debug/andlerd
```

#### Debugging with VS Code

1. **Install VS Code Rust Extension**
   ```bash
   # Install VS Code
   # Install Rust extension: rust-analyzer
   ```

2. **Configure Debug Settings**
   ```json
   {
     "version": "0.2.0",
     "configurations": [
       {
         "type": "lldb",
         "request": "launch",
         "name": "Debug andlerd",
         "cargo": {
           "args": [
             "build",
             "--bin",
             "andlerd",
             "--message-format=json"
           ],
           "filter": {
             "name": "andlerd",
             "kind": "bin"
           }
         },
         "args": [],
         "cwd": "${workspaceFolder}",
         "stopOnEntry": false
       }
     ]
   }
   ```

#### Debugging with IntelliJ IDEA

1. **Install Rust Plugin**
   ```bash
   # Install Rust plugin in IntelliJ IDEA
   ```

2. **Configure Run Configuration**
   ```bash
   # Create Rust run configuration
   # Set main class to andlerd
   # Set working directory to project root
   ```

### Performance Profiling

#### CPU Profiling

```bash
# Install profiling tools
sudo apt install perf

# Profile CPU usage
perf record --call-graph dwarf cargo run -p andler-daemon

# View profiling results
perf report
```

#### Memory Profiling

```bash
# Install memory profiling tools
sudo apt install valgrind

# Profile memory usage
valgrind --tool=memcheck cargo run -p andler-daemon

# Profile memory leaks
valgrind --tool=massif cargo run -p andler-daemon
```

#### Memory Profiling with Rust

```bash
# Install memory profiling tools
cargo install flamegraph

# Profile memory usage
cargo flamegraph --bin andlerd

# Profile memory usage with specific function
cargo flamegraph --bin andlerd -- --function create_instance
```

### Testing Strategy

#### Unit Tests

```rust
// Example unit test for andler-core
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_instance_config_validation() {
        let config = InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm,
            backend: BackendKind::Qemu,
            cpu: CpuConfig {
                cores: 4,
                sockets: 1,
                threads: 1,
                affinity: vec![],
                priority: CpuPriority::Normal,
            },
            memory: MemoryConfig {
                size: 8 * 1024 * 1024 * 1024, // 8GB
                ballooning: false,
                zram: false,
                ksm: false,
            },
            disk: DiskConfig {
                path: PathBuf::from("/tmp/test.qcow2"),
                size: 40 * 1024 * 1024 * 1024, // 40GB
                format: DiskFormat::Qcow2,
                base_image: None,
                thin_provisioning: true,
                trim_on_shutdown: true,
            },
            display: DisplayConfig {
                width: 1920,
                height: 1080,
                dpi: 96,
                fps_limit: 60,
                engine: DisplayEngine::Sdl,
                fullscreen: false,
            },
            render_backend: RenderBackend::Venus,
            network: NetworkConfig {
                mode: NetworkMode::Nat,
                ports: vec![],
                bridge_name: String::new(),
            },
        };
        
        assert!(config.validate().is_ok());
    }
    
    #[test]
    fn test_instance_state_machine() {
        let mut state = InstanceState::Created;
        
        // Test valid transitions
        assert!(state.transition_to(InstanceState::Starting).is_ok());
        assert!(state.transition_to(InstanceState::Running).is_ok());
        assert!(state.transition_to(InstanceState::Paused).is_ok());
        assert!(state.transition_to(InstanceState::Running).is_ok());
        assert!(state.transition_to(InstanceState::Stopping).is_ok());
        assert!(state.transition_to(InstanceState::Stopped).is_ok());
        
        // Test invalid transitions
        assert!(state.transition_to(InstanceState::Running).is_err());
        assert!(state.transition_to(InstanceState::Created).is_err());
    }
}
```

#### Integration Tests

```rust
// Example integration test for andler-qemu
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;
    
    #[tokio::test]
    #[ignore] // Requires KVM
    async fn test_qemu_backend_spawn() {
        let backend = QemuBackend::new();
        let config = InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm,
            backend: BackendKind::Qemu,
            cpu: CpuConfig {
                cores: 2,
                sockets: 1,
                threads: 1,
                affinity: vec![],
                priority: CpuPriority::Normal,
            },
            memory: MemoryConfig {
                size: 4 * 1024 * 1024 * 1024, // 4GB
                ballooning: false,
                zram: false,
                ksm: false,
            },
            disk: DiskConfig {
                path: PathBuf::from("/tmp/test.qcow2"),
                size: 20 * 1024 * 1024 * 1024, // 20GB
                format: DiskFormat::Qcow2,
                base_image: None,
                thin_provisioning: true,
                trim_on_shutdown: true,
            },
            display: DisplayConfig {
                width: 1280,
                height: 720,
                dpi: 96,
                fps_limit: 30,
                engine: DisplayEngine::Sdl,
                fullscreen: false,
            },
            render_backend: RenderBackend::VirtioGpu,
            network: NetworkConfig {
                mode: NetworkMode::Nat,
                ports: vec![],
                bridge_name: String::new(),
            },
        };
        
        let handle = backend.spawn(&config).await;
        assert!(handle.is_ok());
        
        let handle = handle.unwrap();
        let status = backend.status(&handle).await;
        assert!(status.is_ok());
        
        let status = status.unwrap();
        assert_eq!(status.state, BackendStatus::Running);
        
        backend.stop(&handle, true).await.unwrap();
    }
}
```

#### End-to-End Tests

```rust
// Example end-to-end test
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};
    
    #[tokio::test]
    #[ignore] // Requires KVM
    async fn test_vm_lifecycle() {
        let daemon = Daemon::new().await.unwrap();
        
        // Create VM
        let create_request = CreateInstanceRequest {
            name: "e2e-test-vm".to_string(),
            kind: InstanceKind::LinuxVm,
            backend: BackendKind::Qemu,
            cpu: CpuConfig {
                cores: 2,
                sockets: 1,
                threads: 1,
                affinity: vec![],
                priority: CpuPriority::Normal,
            },
            memory: MemoryConfig {
                size: 4 * 1024 * 1024 * 1024, // 4GB
                ballooning: false,
                zram: false,
                ksm: false,
            },
            disk: DiskConfig {
                path: PathBuf::from("/tmp/e2e-test.qcow2"),
                size: 20 * 1024 * 1024 * 1024, // 20GB
                format: DiskFormat::Qcow2,
                base_image: None,
                thin_provisioning: true,
                trim_on_shutdown: true,
            },
            display: DisplayConfig {
                width: 1280,
                height: 720,
                dpi: 96,
                fps_limit: 30,
                engine: DisplayEngine::Sdl,
                fullscreen: false,
            },
            render_backend: RenderBackend::VirtioGpu,
            network: NetworkConfig {
                mode: NetworkMode::Nat,
                ports: vec![],
                bridge_name: String::new(),
            },
        };
        
        let instance = daemon.create_instance(create_request).await.unwrap();
        assert_eq!(instance.state, InstanceState::Created);
        
        // Start VM
        let start_request = StartInstanceRequest {
            instance_id: instance.id.clone(),
        };
        
        let status = daemon.start_instance(start_request).await.unwrap();
        assert!(status.success);
        assert_eq!(status.state, InstanceState::Running);
        
        // Wait for VM to be ready
        let status = timeout(
            Duration::from_secs(30),
            daemon.get_instance_status(GetInstanceStatusRequest {
                instance_id: instance.id.clone(),
            })
        ).await.unwrap().unwrap();
        assert_eq!(status.state, InstanceState::Running);
        
        // Pause VM
        let pause_request = PauseInstanceRequest {
            instance_id: instance.id.clone(),
        };
        
        let status = daemon.pause_instance(pause_request).await.unwrap();
        assert!(status.success);
        assert_eq!(status.state, InstanceState::Paused);
        
        // Resume VM
        let resume_request = ResumeInstanceRequest {
            instance_id: instance.id.clone(),
        };
        
        let status = daemon.resume_instance(resume_request).await.unwrap();
        assert!(status.success);
        assert_eq!(status.state, InstanceState::Running);
        
        // Stop VM
        let stop_request = StopInstanceRequest {
            instance_id: instance.id.clone(),
            graceful: true,
        };
        
        let status = daemon.stop_instance(stop_request).await.unwrap();
        assert!(status.success);
        assert_eq!(status.state, InstanceState::Stopped);
        
        // Remove VM
        let remove_request = RemoveInstanceRequest {
            instance_id: instance.id.clone(),
        };
        
        let status = daemon.remove_instance(remove_request).await.unwrap();
        assert!(status.success);
        
        // Verify VM is removed
        let instances = daemon.list_instances(ListInstancesRequest {
            filter: String::new(),
            include_stopped: true,
        }).await.unwrap();
        assert!(!instances.instances.iter().any(|i| i.id == instance.id));
    }
}
```

### Code Style and Conventions

#### Rust Code Style

```rust
// Use rustfmt for formatting
cargo fmt

// Use clippy for linting
cargo clippy --workspace

// Follow Rust naming conventions
// - Variables: snake_case
// - Functions: snake_case
// - Types: PascalCase
// - Constants: SCREAMING_SNAKE_CASE
// - Modules: snake_case

// Use proper error handling
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AndlerError {
    #[error("Invalid configuration: {message}")]
    InvalidConfig { message: String },
    
    #[error("Instance not found: {id}")]
    InstanceNotFound { id: String },
    
    #[error("Backend error: {message}")]
    BackendError { message: String },
    
    #[error("Disk error: {message}")]
    DiskError { message: String },
    
    #[error("Network error: {message}")]
    NetworkError { message: String },
}

// Use proper async/await patterns
async fn create_instance(config: InstanceConfig) -> Result<InstanceInfo, AndlerError> {
    // Validate configuration
    config.validate()?;
    
    // Create instance
    let instance = Instance::new(config)?;
    
    // Save to database
    instance.save().await?;
    
    // Return instance info
    Ok(instance.info())
}

// Use proper trait implementations
#[async_trait]
impl HypervisorBackend for QemuBackend {
    fn name(&self) -> &'static str {
        "qemu"
    }
    
    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        // Generate command line
        let args = build_args(cfg)?;
        
        // Spawn QEMU process
        let process = QemuProcess::spawn(args)?;
        
        // Return handle
        Ok(BackendHandle {
            id: process.id(),
            backend: self.name().to_string(),
        })
    }
    
    // Implement other required methods...
}
```

#### Documentation

```rust
/// Creates a new VM instance with the given configuration.
///
/// # Arguments
///
/// * `config` - The configuration for the new VM instance
///
/// # Returns
///
/// A `Result` containing the created `InstanceInfo` or an error
///
/// # Examples
///
/// ```rust
/// use andler_core::config::InstanceConfig;
/// use andler_core::error::AndlerError;
///
/// async fn example() -> Result<(), AndlerError> {
///     let config = InstanceConfig {
///         // ... configuration ...
///     };
///     
///     let instance = create_instance(config).await?;
///     println!("Created instance: {}", instance.name);
///     
///     Ok(())
/// }
/// ```
pub async fn create_instance(config: InstanceConfig) -> Result<InstanceInfo, AndlerError> {
    // Implementation...
}

/// Represents a virtual machine instance.
///
/// # Examples
///
/// ```rust
/// use andler_core::instance::Instance;
///
/// let instance = Instance::new(config)?;
/// instance.start()?;
/// instance.stop()?;
/// ```
pub struct Instance {
    id: InstanceId,
    name: String,
    config: InstanceConfig,
    state: InstanceState,
}
```

#### Testing Conventions

```rust
// Test naming conventions
#[test]
fn test_instance_config_validation() {
    // Test implementation...
}

#[test]
fn test_instance_state_machine() {
    // Test implementation...
}

// Use proper test attributes
#[cfg(test)]
mod tests {
    use super::*;
    
    // Use #[ignore] for tests that require external resources
    #[tokio::test]
    #[ignore] // Requires KVM
    async fn test_qemu_backend_spawn() {
        // Test implementation...
    }
    
    // Use proper test setup and teardown
    #[test]
    fn test_disk_operations() {
        let temp_dir = tempfile::tempdir().unwrap();
        let disk_path = temp_dir.path().join("test.qcow2");
        
        // Test implementation...
        
        // Cleanup
        drop(temp_dir);
    }
}
```

### CI/CD Pipeline

#### GitHub Actions

```yaml
name: CI/CD

on:
  push:
    branches: [ main, develop ]
  pull_request:
    branches: [ main ]

jobs:
  test:
    runs-on: ubuntu-latest
    
    steps:
    - uses: actions/checkout@v3
    
    - name: Install Rust
      uses: dtolnay/rust-toolchain@stable
      with:
        components: rustfmt, clippy
    
    - name: Install dependencies
      run: |
        sudo apt update
        sudo apt install -y qemu-system-x86 qemu-utils ovmf
    
    - name: Build
      run: cargo build --workspace
    
    - name: Run tests
      run: cargo test --workspace
    
    - name: Run integration tests
      run: cargo test --workspace -- --ignored
      if: github.event_name == 'push'
    
    - name: Check formatting
      run: cargo fmt --check
    
    - name: Run clippy
      run: cargo clippy --workspace -- -D warnings

  build:
    needs: test
    runs-on: ubuntu-latest
    
    steps:
    - uses: actions/checkout@v3
    
    - name: Install Rust
      uses: dtolnay/rust-toolchain@stable
    
    - name: Build release
      run: cargo build --release --workspace
    
    - name: Upload artifacts
      uses: actions/upload-artifact@v3
      with:
        name: andler-artifacts
        path: target/release/
```

#### Docker CI/CD

```dockerfile
# syntax=docker/dockerfile:1

# Build stage
FROM rust:bookworm AS builder

WORKDIR /andler

# Install system dependencies
RUN apt update && apt install -y \
    qemu-system-x86 \
    qemu-utils \
    ovmf \
    libssl-dev \
    libz-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy project files
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build the project
RUN cargo build --release --workspace

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt update && apt install -y \
    qemu-system-x86 \
    qemu-utils \
    ovmf \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy built binaries
COPY --from=builder /andler/target/release/andlerd /usr/local/bin/
COPY --from=builder /andler/target/release/andler /usr/local/bin/

# Create user and group
RUN groupadd -r andler && useradd -r -g andler andler

# Set up directories
RUN mkdir -p /var/lib/andler /var/log/andler && \
    chown andler:andler /var/lib/andler /var/log/andler

# Expose ports
EXPOSE 50051

# Set environment variables
ENV ANDLER_DATA_DIR=/var/lib/andler
ENV ANDLER_LOG_DIR=/var/log/andler

# Run as non-root user
USER andler

# Start the daemon
ENTRYPOINT ["/usr/local/bin/andlerd"]
```

### Deployment

#### Development Deployment

```bash
# Build and run in development mode
cargo run -p andler-daemon -- --dev

# Run with Docker
docker compose up daemon

# Run with Docker in detached mode
docker compose up -d daemon
```

#### Production Deployment

```bash
# Build release version
cargo build --release --workspace

# Install system dependencies
sudo apt install qemu-system-x86 qemu-utils ovmf

# Create user and group
sudo groupadd andler
sudo useradd -r -g andler -s /bin/false andler

# Create directories
sudo mkdir -p /var/lib/andler /var/log/andler
sudo chown andler:andler /var/lib/andler /var/log/andler

# Copy binaries
sudo cp target/release/andlerd /usr/local/bin/
sudo cp target/release/andler /usr/local/bin/

# Create systemd service
sudo tee /etc/systemd/system/andlerd.service > /dev/null << EOF
[Unit]
Description=ANDLER Daemon
After=network.target

[Service]
Type=simple
User=andler
Group=andler
ExecStart=/usr/local/bin/andlerd
Restart=on-failure
RestartSec=5
Environment=ANDLER_DATA_DIR=/var/lib/andler
Environment=ANDLER_LOG_DIR=/var/log/andler

[Install]
WantedBy=multi-user.target
EOF

# Enable and start service
sudo systemctl daemon-reload
sudo systemctl enable andlerd
sudo systemctl start andlerd

# Check service status
sudo systemctl status andlerd
```

#### Docker Deployment

```bash
# Build Docker image
docker build -t andler/daemon .

# Run Docker container
docker run -d \
    --name andlerd \
    --device=/dev/kvm \
    -p 50051:50051 \
    -v andler-data:/var/lib/andler \
    -v andler-log:/var/log/andler \
    andler/daemon

# Check container status
docker ps

# View logs
docker logs andlerd

# Stop container
docker stop andlerd

# Remove container
docker rm andlerd
```

## Advanced Topics

### Adding a New Backend

1. **Create Backend Crate**
   ```bash
   mkdir -p crates/andler-mybackend
   cd crates/andler-mybackend
   cargo init --lib
   ```

2. **Implement Backend Trait**
   ```rust
   use andler_core::backend::{HypervisorBackend, BackendError, BackendHandle};
   use async_trait::async_trait;
   
   #[async_trait]
   impl HypervisorBackend for MyBackend {
       fn name(&self) -> &'static str {
           "mybackend"
       }
       
       async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
           // Implementation here
           todo!()
       }
       
       async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError> {
           Err(BackendError::NotImplemented { 
               backend: "mybackend", 
               operation: "pause" 
           })
       }
       
       async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError> {
           Err(BackendError::NotImplemented { 
               backend: "mybackend", 
               operation: "resume" 
           })
       }
       
       async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError> {
           Err(BackendError::NotImplemented { 
               backend: "mybackend", 
               operation: "stop" 
           })
       }
       
       async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError> {
           Err(BackendError::NotImplemented { 
               backend: "mybackend", 
               operation: "status" 
           })
       }
       
       async fn snapshot(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError> {
           Err(BackendError::NotImplemented { 
               backend: "mybackend", 
               operation: "snapshot" 
           })
       }
       
       fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics> {
           Box::pin(futures::stream::empty())
       }
   }
   ```

3. **Register Backend in Daemon**
   ```rust
   // In andler-daemon/src/main.rs
   use andler_mybackend::MyBackend;
   
   fn register_backends(registry: &mut BackendRegistry) {
       registry.register(Box::new(MyBackend::new()));
   }
   ```

4. **Add to Workspace**
   ```toml
   # In Cargo.toml
   [workspace]
   members = [
       "crates/andler-core",
       "crates/andler-qemu",
       "crates/andler-vmm",
       "crates/andler-disk",
       "crates/andler-net",
       "crates/andler-store",
       "crates/andler-rpc",
       "crates/andler-daemon",
       "crates/andler-cli",
       "crates/andler-mybackend",  # Add new backend
   ]
   ```

### Extending the gRPC API

1. **Define New RPC Method**
   ```protobuf
   // In andler-rpc/proto/andler.proto
   service AndlerService {
       // Existing methods...
       
       // New method
       rpc UpdateInstanceConfig(UpdateInstanceConfigRequest) returns (StatusResponse);
   }
   
   message UpdateInstanceConfigRequest {
       string instance_id = 1;
       CpuConfig cpu = 2;
       MemoryConfig memory = 3;
       DiskConfig disk = 4;
       DisplayConfig display = 5;
       NetworkConfig network = 6;
   }
   ```

2. **Implement RPC Method**
   ```rust
   // In andler-daemon/src/service.rs
   #[tonic::async_trait]
   impl AndlerService for AndlerServiceImpl {
       async fn update_instance_config(
           &self,
           request: Request<UpdateInstanceConfigRequest>,
       ) -> Result<Response<StatusResponse>, Status> {
           let request = request.into_inner();
           
           // Validate request
           if request.instance_id.is_empty() {
               return Err(Status::invalid_argument("instance_id is required"));
           }
           
           // Get instance
           let instance = self.get_instance(&request.instance_id).await?;
           
           // Update configuration
           let mut config = instance.config.clone();
           if let Some(cpu) = request.cpu {
               config.cpu = cpu;
           }
           if let Some(memory) = request.memory {
               config.memory = memory;
           }
           if let Some(disk) = request.disk {
               config.disk = disk;
           }
           if let Some(display) = request.display {
               config.display = display;
           }
           if let Some(network) = request.network {
               config.network = network;
           }
           
           // Save configuration
           instance.update_config(config).await?;
           
           Ok(Response::new(StatusResponse {
               success: true,
               message: "Configuration updated successfully".to_string(),
               state: instance.state(),
           }))
       }
   }
   ```

3. **Add CLI Command**
   ```bash
   # In andler-cli/src/commands/update_config.rs
   use clap::Args;
   use andler_cli::client::AndlerClient;
   
   #[derive(Args)]
   pub struct UpdateConfigArgs {
       #[arg(short, long)]
       pub instance_id: String,
       
       #[arg(short, long)]
       pub cpu: Option<u32>,
       
       #[arg(short, long)]
       pub memory: Option<String>,
       
       #[arg(short, long)]
       pub disk: Option<String>,
       
       #[arg(short, long)]
       pub width: Option<u32>,
       
       #[arg(short, long)]
       pub height: Option<u32>,
   }
   
   pub async fn update_config(client: &mut AndlerClient, args: UpdateConfigArgs) -> Result<(), Box<dyn std::error::Error>> {
       // Build request
       let mut request = UpdateInstanceConfigRequest::default();
       request.instance_id = args.instance_id.clone();
       
       if let Some(cpu) = args.cpu {
           request.cpu = Some(CpuConfig {
               cores: cpu,
               ..Default::default()
           });
       }
       
       if let Some(memory) = args.memory {
           request.memory = Some(MemoryConfig {
               size: parse_memory_size(&memory)?,
               ..Default::default()
           });
       }
       
       if let Some(disk) = args.disk {
           request.disk = Some(DiskConfig {
               size: parse_disk_size(&disk)?,
               ..Default::default()
           });
       }
       
       if let Some(width) = args.width {
           if let Some(height) = args.height {
               request.display = Some(DisplayConfig {
                   width,
                   height,
                   ..Default::default()
               });
           }
       }
       
       // Send request
       let response = client.update_instance_config(request).await?;
       
       // Print response
       println!("Status: {}", response.message);
       
       Ok(())
   }
   ```

### Performance Optimization

#### CPU Optimization

```rust
// Optimize CPU usage with affinity
pub fn optimize_cpu_affinity(&self, cores: &[usize]) -> Result<(), AndlerError> {
    // Set CPU affinity for QEMU process
    let mut command = Command::new("taskset");
    command.arg("-cp").arg(cores.join(","));
    command.arg(self.qemu_process.id().to_string());
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::CpuError {
            message: format!("Failed to set CPU affinity: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}

// Optimize CPU priority
pub fn optimize_cpu_priority(&self, priority: CpuPriority) -> Result<(), AndlerError> {
    // Set CPU priority for QEMU process
    let mut command = Command::new("renice");
    command.arg(priority.as_nice_value()).arg(self.qemu_process.id().to_string());
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::CpuError {
            message: format!("Failed to set CPU priority: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}
```

#### Memory Optimization

```rust
// Optimize memory usage with ballooning
pub fn optimize_memory_ballooning(&self, enabled: bool) -> Result<(), AndlerError> {
    // Enable/disable memory ballooning
    let mut command = Command::new("qemu-monitor");
    command.arg("balloon");
    command.arg(if enabled { "on" } else { "off" });
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::MemoryError {
            message: format!("Failed to set memory ballooning: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}

// Optimize memory usage with KSM
pub fn optimize_memory_ksm(&self, enabled: bool) -> Result<(), AndlerError> {
    // Enable/disable KSM
    let mut command = Command::new("sysctl");
    command.arg("-w");
    command.arg(format!("vm.ksm.run={}", if enabled { "1" } else { "0" }));
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::MemoryError {
            message: format!("Failed to set KSM: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}
```

#### Disk Optimization

```rust
// Optimize disk usage with thin provisioning
pub fn optimize_disk_thin_provisioning(&self, enabled: bool) -> Result<(), AndlerError> {
    // Enable/disable thin provisioning
    let mut command = Command::new("qemu-img");
    command.arg("resize");
    command.arg(&self.disk_path);
    command.arg(if enabled { "0" } else { "auto" });
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::DiskError {
            message: format!("Failed to set thin provisioning: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}

// Optimize disk usage with TRIM
pub fn optimize_disk_trim(&self, enabled: bool) -> Result<(), AndlerError> {
    // Enable/disable TRIM on shutdown
    let mut command = Command::new("qemu-img");
    command.arg("convert");
    command.arg("-O");
    command.arg("qcow2");
    command.arg(&self.disk_path);
    command.arg(if enabled { "-c" } else { "" });
    command.arg(&format!("{}_trimmed", self.disk_path.display()));
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::DiskError {
            message: format!("Failed to set TRIM: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}
```

#### Network Optimization

```rust
// Optimize network usage with bandwidth limits
pub fn optimize_network_bandwidth(&self, bandwidth_limit: u64) -> Result<(), AndlerError> {
    // Set bandwidth limit for network interface
    let mut command = Command::new("tc");
    command.arg("qdisc");
    command.arg("add");
    command.arg("dev");
    command.arg(&self.network_interface);
    command.arg("root");
    command.arg("htb");
    command.arg("default");
    command.arg("10");
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::NetworkError {
            message: format!("Failed to set bandwidth limit: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}

// Optimize network usage with QoS
pub fn optimize_network_qos(&self, qos_class: u32) -> Result<(), AndlerError> {
    // Set QoS class for network interface
    let mut command = Command::new("tc");
    command.arg("class");
    command.arg("add");
    command.arg("dev");
    command.arg(&self.network_interface);
    command.arg("parent");
    command.arg("1:0");
    command.arg("classid");
    command.arg(&qos_class.to_string());
    command.arg("htb");
    command.arg("rate");
    command.arg("10mbit");
    command.arg("ceil");
    command.arg("100mbit");
    
    let output = command.output()?;
    if !output.status.success() {
        return Err(AndlerError::NetworkError {
            message: format!("Failed to set QoS: {}", String::from_utf8_lossy(&output.stderr)),
        });
    }
    
    Ok(())
}
```

## Troubleshooting

### Common Issues

#### KVM Permission Denied

```bash
# Add user to kvm group
sudo usermod -aG kvm $USER

# Log out and back in for changes to take effect
# Or restart the system

# Verify permissions
ls -la /dev/kvm
```

#### QEMU Not Found

```bash
# Install QEMU packages
sudo apt install qemu-system-x86 qemu-utils  # Debian/Ubuntu
sudo dnf install qemu-kvm qemu-img           # Fedora

# Verify installation
qemu-system-x86_64 --version
```

#### Docker Build Fails

```bash
# Clean Docker cache
docker system prune -f

# Rebuild without cache
docker compose build --no-cache

# Check Docker logs
docker logs <container-id>
```

#### VM Creation Fails

```bash
# Check QEMU logs
dmesg | grep -i qemu

# Check KVM availability
ls -la /dev/kvm

# Check disk space
df -h

# Check memory availability
free -h
```

#### Network Issues

```bash
# Check network configuration
ip addr show

# Check network interfaces
ip link show

# Check network routes
ip route show

# Check network services
systemctl status networking
```

#### Performance Issues

```bash
# Check CPU usage
top -p $(pgrep qemu-system-x86_64)

# Check memory usage
free -h

# Check disk I/O
iostat -x 1

# Check network I/O
iftop

# Check GPU usage
nvidia-smi
```

### Debugging

#### Enable Debug Logging

```bash
# Enable debug logging
export RUST_LOG=debug
cargo run -p andler-daemon

# Enable debug logging for specific component
export RUST_LOG=andler_core=debug
cargo run -p andler-daemon
```

#### Check VM Logs

```bash
# Check VM logs
journalctl -u andlerd -f

# Check QEMU logs
dmesg | grep -i qemu

# Check system logs
journalctl -f
```

#### Check System Resources

```bash
# Check CPU usage
top

# Check memory usage
free -h

# Check disk usage
df -h

# Check network usage
iftop

# Check GPU usage
nvidia-smi
```

#### Check Network Configuration

```bash
# Check network interfaces
ip addr show

# Check network routes
ip route show

# Check network services
systemctl status networking

# Check network configuration
cat /etc/network/interfaces
```

### Performance Tuning

#### CPU Tuning

```bash
# Set CPU governor to performance
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

# Set CPU frequency to maximum
echo 1 | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/boost

# Set CPU affinity for QEMU process
taskset -cp 0-3 $(pgrep qemu-system-x86_64)
```

#### Memory Tuning

```bash
# Enable KSM
echo 1 | sudo tee /sys/kernel/mm/ksm/run

# Set KSM sleep milliseconds
echo 10 | sudo tee /sys/kernel/mm/ksm/sleep_millisecs

# Enable memory ballooning
echo 1 | sudo tee /sys/devices/system/node/node*/hugepages/hugepages-2048kB/nr_hugepages
```

#### Disk Tuning

```bash
# Set I/O scheduler to deadline
echo deadline | sudo tee /sys/block/sda/queue/scheduler

# Set read-ahead to maximum
blockdev --setra 8192 /dev/sda

# Enable TRIM
fstrim -v /
```

#### Network Tuning

```bash
# Set network buffer size
sysctl -w net.core.rmem_max=16777216
sysctl -w net.core.wmem_max=16777216

# Set TCP buffer size
sysctl -w net.ipv4.tcp_rmem="4096 87380 16777216"
sysctl -w net.ipv4.tcp_wmem="4096 65536 16777216"

# Set network buffer size
sysctl -w net.core.rmem_default=16777216
sysctl -w net.core.wmem_default=16777216
```

## Contributing

### Getting Started

1. **Fork the Repository**
   ```bash
   git clone https://github.com/your-username/andler.git
   cd andler
   ```

2. **Create a Feature Branch**
   ```bash
   git checkout -b feature/my-feature
   ```

3. **Make Changes**
   ```bash
   # Make your changes
   # Add tests
   # Update documentation
   ```

4. **Run Tests**
   ```bash
   # Run all tests
   cargo test --workspace
   
   # Run integration tests (requires KVM)
   cargo test --workspace -- --ignored
   ```

5. **Check Code Quality**
   ```bash
   # Format code
   cargo fmt
   
   # Run clippy
   cargo clippy --workspace -- -D warnings
   ```

6. **Commit Changes**
   ```bash
   git add .
   git commit -m "Add my feature"
   ```

7. **Push Changes**
   ```bash
   git push origin feature/my-feature
   ```

8. **Create Pull Request**
   - Go to GitHub
   - Create a pull request
   - Add description and reviewers

### Code Review Process

1. **Automated Checks**
   - Code formatting (cargo fmt)
   - Code linting (cargo clippy)
   - Unit tests (cargo test)
   - Integration tests (cargo test -- --ignored)

2. **Manual Review**
   - Code quality and style
   - Test coverage
   - Documentation
   - Performance impact

3. **Feedback and Iteration**
   - Address review comments
   - Make necessary changes
   - Re-run tests
   - Update documentation

4. **Approval and Merge**
   - Get approval from maintainers
   - Merge pull request
   - Update documentation

### Pull Request Guidelines

#### Pull Request Template

```markdown
## Description

<!-- Describe your changes in detail -->

## Type of Change

<!-- Mark any that apply -->
- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] Documentation update

## Testing

<!-- Describe the tests you ran to verify your changes -->
- [ ] Unit tests
- [ ] Integration tests
- [ ] End-to-end tests

## Checklist

<!-- Mark any that apply -->
- [ ] My code follows the code style of this project
- [ ] I have added tests to cover my changes
- [ ] I have updated the documentation accordingly
- [ ] My changes generate no new warnings
- [ ] I have added necessary documentation (if appropriate)

## Related Issues

<!-- Link any related issues -->
Closes #123
```

#### Pull Request Checklist

- [ ] Code follows project style guidelines
- [ ] Code is properly formatted (cargo fmt)
- [ ] Code passes linting (cargo clippy)
- [ ] Tests are added/updated
- [ ] Tests pass (cargo test)
- [ ] Documentation is updated
- [ ] No breaking changes (unless explicitly marked)
- [ ] Performance impact is considered
- [ ] Security implications are considered

### Documentation Guidelines

#### Documentation Structure

```
docs/
├── architecture/         # Architecture documentation
├── api/                 # API documentation
├── development/         # Development documentation
├── contributing/        # Contributing documentation
├── reference/           # Reference documentation
└── ...                  # Other documentation
```

#### Documentation Standards

1. **Markdown Format**
   - Use Markdown for all documentation
   - Use proper headings and structure
   - Include code examples where appropriate

2. **Code Examples**
   - Include working code examples
   - Use proper syntax highlighting
   - Test code examples before including

3. **API Documentation**
   - Document all public APIs
   - Include parameter descriptions
   - Include return value descriptions
   - Include usage examples

4. **Architecture Documentation**
   - Document system architecture
   - Document component interactions
   - Document data flow
   - Document security considerations

5. **Development Documentation**
   - Document development setup
   - Document build process
   - Document testing process
   - Document deployment process

#### Documentation Tools

1. **Markdown**
   - Use Markdown for all documentation
   - Use proper headings and structure
   - Include code examples where appropriate

2. **PlantUML**
   - Use PlantUML for architecture diagrams
   - Use PlantUML for sequence diagrams
   - Use PlantUML for state diagrams

3. **Mermaid**
   - Use Mermaid for flowcharts
   - Use Mermaid for Gantt charts
   - Use Mermaid for mindmaps

4. **OpenAPI**
   - Use OpenAPI for API documentation
   - Use OpenAPI for API specifications
   - Use OpenAPI for API examples

### Release Process

#### Release Checklist

1. **Pre-Release**
   - [ ] Run all tests
   - [ ] Run integration tests
   - [ ] Run code quality checks
   - [ ] Update documentation
   - [ ] Update changelog
   - [ ] Update version numbers

2. **Release**
   - [ ] Create release branch
   - [ ] Build release artifacts
   - [ ] Create release notes
   - [ ] Tag release
   - [ ] Publish release

3. **Post-Release**
   - [ ] Update documentation
   - [ ] Update changelog
   - [ ] Update version numbers
   - [ ] Announce release
   - [ ] Monitor for issues

#### Release Notes Template

```markdown
# Release Notes

## Version X.Y.Z

### Date
YYYY-MM-DD

### Changes
- **Feature**: Description of new feature
- **Bug Fix**: Description of bug fix
- **Improvement**: Description of improvement
- **Breaking Change**: Description of breaking change

### Contributors
- @username1
- @username2
- @username3

### Links
- [GitHub Releases](https://github.com/andler-project/andler/releases/tag/vX.Y.Z)
- [Documentation](https://docs.andler.dev)
- [Changelog](https://github.com/andler-project/andler/blob/main/CHANGELOG.md)
```

## FAQ

### Common Questions

#### How do I install ANDLER?

```bash
# From source
git clone https://github.com/andler-project/andler.git
cd andler
cargo build --release
sudo cp target/release/andlerd /usr/local/bin/
sudo cp target/release/andler /usr/local/bin/

# From package
# Debian/Ubuntu
sudo apt install andler

# Fedora/RHEL
sudo dnf install andler

# Arch Linux
sudo pacman -S andler
```

#### How do I create a VM?

```bash
# Create a Linux VM
andler create --name my-vm --type linux --iso /path/to/iso

# Create an Android VM
andler create --name my-android-vm --type android

# Create a VM with specific configuration
andler create --name my-vm --type linux --iso /path/to/iso \
    --cpu 4 --memory 8G --disk 40G \
    --render-backend venus --network nat
```

#### How do I manage VMs?

```bash
# Start a VM
andler start my-vm

# Stop a VM
andler stop my-vm

# Pause a VM
andler pause my-vm

# Resume a VM
andler resume my-vm

# Clone a VM
andler clone --name new-vm my-vm

# Remove a VM
andler remove my-vm
```

#### How do I monitor VMs?

```bash
# Get VM status
andler status my-vm

# Get VM metrics
andler metrics my-vm

# Stream VM metrics
andler metrics --stream my-vm

# Stream VM logs
andler logs --stream my-vm
```

#### How do I troubleshoot issues?

```bash
# Check VM status
andler status --verbose my-vm

# Check VM logs
andler logs --stream my-vm

# Check system resources
free -h
df -h
top

# Check network configuration
ip addr show
ip route show

# Check KVM availability
ls -la /dev/kvm
```

#### How do I optimize performance?

```bash
# Optimize CPU usage
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor

# Optimize memory usage
echo 1 | sudo tee /sys/kernel/mm/ksm/run

# Optimize disk usage
echo deadline | sudo tee /sys/block/sda/queue/scheduler

# Optimize network usage
sysctl -w net.core.rmem_max=16777216
sysctl -w net.core.wmem_max=16777216
```

#### How do I contribute?

```bash
# Fork the repository
git clone https://github.com/your-username/andler.git
cd andler

# Create a feature branch
git checkout -b feature/my-feature

# Make changes
# Add tests
# Update documentation

# Run tests
cargo test --workspace

# Check code quality
cargo fmt
cargo clippy --workspace -- -D warnings

# Commit changes
git add .
git commit -m "Add my feature"

# Push changes
git push origin feature/my-feature

# Create pull request
# Go to GitHub and create a pull request
```

#### How do I get help?

- **Documentation**: https://docs.andler.dev
- **GitHub Issues**: https://github.com/andler-project/andler/issues
- **GitHub Discussions**: https://github.com/andler-project/andler/discussions
- **Discord**: https://discord.gg/andler
- **Email**: support@andler.dev

## License

ANDLER is licensed under the MIT License. See the [LICENSE](../LICENSE) file for details.

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
- **Discord**: [https://discord.gg/andler](https://discord.gg/andler)