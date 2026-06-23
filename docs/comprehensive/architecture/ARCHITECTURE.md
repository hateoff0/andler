# Architecture Documentation

## System Architecture

ANDLER follows a layered architecture with clear separation of concerns:

```
┌─────────────────────────────────────────────────────────────┐
│                    Presentation Layer                      │
├─────────────────────────────────────────────────────────────┤
│  Frontend (Tauri GUI)  │  CLI (Command Line Interface)    │
├─────────────────────────────────────────────────────────────┤
│                    API Layer                               │
├─────────────────────────────────────────────────────────────┤
│  gRPC API (andler-rpc)                                   │
├─────────────────────────────────────────────────────────────┤
│                    Service Layer                           │
├─────────────────────────────────────────────────────────────┤
│  Daemon (andlerd)                                         │
├─────────────────────────────────────────────────────────────┤
│                    Domain Layer                            │
├─────────────────────────────────────────────────────────────┤
│  Core Logic (andler-core)                                 │
├─────────────────────────────────────────────────────────────┤
│                    Infrastructure Layer                    │
├─────────────────────────────────────────────────────────────┤
│  Backend Implementations (QEMU, rust-vmm)                 │
├─────────────────────────────────────────────────────────────┤
│                    System Layer                            │
├─────────────────────────────────────────────────────────────┤
│  QEMU, KVM, OVMF, and other system dependencies           │
└─────────────────────────────────────────────────────────────┘
```

## Component Architecture

### Core Components

#### 1. andler-core (Domain Layer)

**Purpose**: Central domain logic and business rules

**Key Responsibilities**:
- Instance configuration management
- State machine implementation
- Backend abstraction
- Resource management logic

**Key Types**:
```rust
// Instance configuration
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

// Backend abstraction
pub trait HypervisorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn supported_render_backends(&self) -> &[RenderBackend];
    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError>;
    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError>;
    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError>;
    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError>;
    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError>;
    async fn snapshot(&self, handle: &BackendHandle, tag: &str) -> Result<(), BackendError>;
    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics>;
}
```

**State Machine**:
```
Created → Starting → Running ⇄ Paused
                         │
                         ▼
                      Stopping → Stopped
                         │
                         ▼
                       Error
```

#### 2. andler-qemu (Backend Implementation)

**Purpose**: QEMU-based VM execution backend

**Key Responsibilities**:
- QEMU process management
- Command-line generation
- QMP (QEMU Machine Protocol) client
- GPU configuration

**Key Modules**:
- `cmdline.rs`: QEMU command-line generation
- `process.rs`: QEMU process management
- `backend.rs`: HypervisorBackend implementation
- `qmp.rs`: QMP client (future)

**GPU Configuration**:
```rust
// Venus (Vulkan proxy) - maximum performance
- device virtio-gpu-gl,hostmem=4096M,blob=true,venus=true
- display sdl,gl=on,show-cursor=off

// VirtIO-GPU - universal option
- device virtio-gpu-pci

// CPU rendering - software fallback
- vga std
```

#### 3. andler-disk (Storage Management)

**Purpose**: Disk image management and operations

**Key Responsibilities**:
- qcow2 disk operations
- Overlay disk management for Android VMs
- Disk resizing and compression
- Snapshot management

**Key Operations**:
```rust
// Basic operations
pub async fn create(path: &Path, size: u64) -> Result<(), DiskError>;
pub async fn create_with_backing_file(path: &Path, backing: &Path, size: u64) -> Result<(), DiskError>;
pub async fn clone_full(source: &Path, dest: &Path) -> Result<(), DiskError>;
pub async fn resize(path: &Path, new_size: u64) -> Result<(), DiskError>;
pub async fn compact(path: &Path) -> Result<(), DiskError>;

// Overlay operations for Android VMs
pub async fn create_overlay(base: &Path, overlay: &Path) -> Result<(), DiskError>;
pub async fn factory_reset(overlay: &Path) -> Result<(), DiskError>;
```

#### 4. andler-net (Network Configuration)

**Purpose**: Network configuration and management

**Key Responsibilities**:
- NAT network configuration
- Bridge network setup
- Network isolation
- Port forwarding

**Network Modes**:
```rust
pub enum NetworkMode {
    Nat,           // Default, isolated network
    Bridge,        // Bridge to host network
    Isolated,      // No network access
}
```

#### 5. andler-store (Persistence)

**Purpose**: Persistent storage for VM state

**Key Responsibilities**:
- SQLite database management
- VM state persistence
- Configuration storage
- Migration management

**Database Schema**:
```sql
-- VM instances table
CREATE TABLE instances (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    state TEXT NOT NULL,
    config TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- Snapshots table
CREATE TABLE snapshots (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (instance_id) REFERENCES instances(id)
);
```

#### 6. andler-rpc (API Layer)

**Purpose**: gRPC API for component communication

**Key Responsibilities**:
- Protocol buffer definitions
- gRPC server implementation
- Client generation
- API versioning

**Service Definition**:
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

#### 7. andler-daemon (Service Layer)

**Purpose**: Background service managing VM lifecycle

**Key Responsibilities**:
- Backend registration and management
- VM lifecycle management
- Resource monitoring
- Configuration management

**Backend Registry**:
```rust
pub struct BackendRegistry {
    backends: HashMap<String, Box<dyn HypervisorBackend>>,
}

impl BackendRegistry {
    pub fn register(&mut self, backend: Box<dyn HypervisorBackend>) {
        self.backends.insert(backend.name().to_string(), backend);
    }
    
    pub fn get(&self, name: &str) -> Option<&dyn HypervisorBackend> {
        self.backends.get(name).map(|b| b.as_ref())
    }
}
```

#### 8. andler-cli (Presentation Layer)

**Purpose**: Command-line interface for VM management

**Key Responsibilities**:
- Command parsing and execution
- User interaction
- Output formatting
- Configuration management

**CLI Commands**:
```bash
# VM Management
andler create --name my-vm --type linux --iso /path/to/iso
andler start my-vm
andler stop my-vm
andler pause my-vm
andler resume my-vm
andler clone --name new-vm my-vm
andler remove my-vm
andler list
andler status my-vm

# Configuration
andler config --get
andler config --set key=value

# Snapshots
andler snapshot --create --tag backup my-vm
andler snapshot --restore --tag backup my-vm
```

#### 9. andler-vmm (Future Backend)

**Purpose**: rust-vmm backend implementation (future)

**Current Status**: Placeholder implementation

**Key Responsibilities**:
- rust-vmm integration
- Alternative VM execution backend

**Implementation**:
```rust
#[async_trait]
impl HypervisorBackend for VmmBackend {
    fn name(&self) -> &'static str {
        "vmm"
    }
    
    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        Err(BackendError::NotImplemented { 
            backend: "vmm", 
            operation: "spawn" 
        })
    }
    
    // All other methods return NotImplemented error
}
```

### Future Components

#### Frontend (Tauri GUI)

**Purpose**: Graphical user interface for VM management

**Key Responsibilities**:
- VM visualization and management
- Resource monitoring dashboard
- Configuration interface
- Real-time VM status updates

**Technology Stack**:
- Tauri (Rust backend)
- TypeScript/React or Svelte (frontend)
- WebSocket for real-time updates

**Architecture**:
```
frontend/
├── src-tauri/          # Rust backend
│   ├── src/
│   │   ├── main.rs
│   │   ├── rpc_client.rs
│   │   └── commands.rs
│   └── Cargo.toml
├── src/                # TypeScript frontend
│   ├── components/
│   │   ├── InstanceList/
│   │   ├── InstanceCreateWizard/
│   │   ├── InstanceLiveView/
│   │   ├── ResourceMonitor/
│   │   └── Settings/
│   ├── api/
│   └── App.tsx
└── package.json
```

#### Guest Image Builder

**Purpose**: Build and manage guest images for VMs

**Key Responsibilities**:
- Guest image building
- Android image provisioning
- Image versioning and distribution
- Image validation

**Architecture**:
```
guest-image/
├── build/              # Image building pipeline
│   ├── base/           # Base Linux image
│   ├── waydroid/       # Android provisioning
│   ├── translators/    # Architecture translators
│   └── network/        # Network configuration
├── variants/           # Image variants
├── manifests/          # Version manifests
└── ci/                 # CI/CD pipeline
```

#### Presets Manager

**Purpose**: Manage VM presets and configurations

**Key Responsibilities**:
- Preset management
- Configuration templates
- Game-specific configurations
- Preset validation

**Architecture**:
```
presets/
├── schema/             # JSON Schema for validation
├── presets/            # Preset files
│   ├── game-1.toml
│   ├── game-2.toml
│   └── ...
└── templates/          # Configuration templates
```

#### Packaging System

**Purpose**: Distribution and packaging of ANDLER

**Key Responsibilities**:
- Package building
- Distribution management
- Platform-specific packaging
- Update management

**Architecture**:
```
packaging/
├── appimage/           # AppImage packaging
├── deb/                # Debian/Ubuntu packaging
├── rpm/                # Fedora/RHEL packaging
├── flatpak/            # Flatpak packaging
└── aur/                # Arch User Repository
```

## Data Flow

### VM Creation Flow

```
1. User requests VM creation via CLI/GUI
2. CLI/GUI sends CreateInstanceRequest to daemon via gRPC
3. Daemon validates request and creates InstanceConfig
4. Daemon selects appropriate backend (QEMU/rust-vmm)
5. Backend generates VM configuration and command-line
6. Backend spawns VM process
7. Daemon updates VM state and persists to database
8. Daemon returns InstanceInfo to client
9. Client displays VM status and configuration
```

### VM Lifecycle Flow

```
1. User requests VM start via CLI/GUI
2. CLI/GUI sends StartInstanceRequest to daemon via gRPC
3. Daemon validates VM state (must be Created or Stopped)
4. Daemon selects appropriate backend
5. Backend generates VM configuration and command-line
6. Backend spawns VM process
7. Daemon updates VM state to Running
8. Daemon starts monitoring VM metrics
9. Daemon returns success to client
10. Client displays VM status and metrics
```

## API Architecture

### gRPC Protocol

#### Service Interface

```protobuf
service AndlerService {
    // VM Management
    rpc CreateInstance(CreateInstanceRequest) returns (InstanceInfo);
    rpc StartInstance(StartInstanceRequest) returns (StatusResponse);
    rpc StopInstance(StopInstanceRequest) returns (StatusResponse);
    rpc PauseInstance(PauseInstanceRequest) returns (StatusResponse);
    rpc ResumeInstance(ResumeInstanceRequest) returns (StatusResponse);
    rpc CloneInstance(CloneInstanceRequest) returns (InstanceInfo);
    rpc RemoveInstance(RemoveInstanceRequest) returns (StatusResponse);
    
    // VM Information
    rpc ListInstances(ListInstancesRequest) returns (InstanceList);
    rpc GetInstanceStatus(GetInstanceStatusRequest) returns (InstanceInfo);
    
    // VM Monitoring
    rpc StreamInstanceLogs(StreamInstanceLogsRequest) returns (stream LogEntry);
    rpc StreamResourceMetrics(StreamResourceMetricsRequest) returns (stream ResourceMetrics);
}
```

#### Request/Response Types

```protobuf
// VM Configuration Types
message CpuConfig {
    uint32 cores = 1;
    uint32 sockets = 2;
    uint32 threads = 3;
    repeated uint32 affinity = 4;
    CpuPriority priority = 5;
}

message MemoryConfig {
    uint64 size = 1;  // in bytes
    bool ballooning = 2;
    bool zram = 3;
    bool ksm = 4;
}

message DiskConfig {
    string path = 1;
    uint64 size = 2;  // in bytes
    DiskFormat format = 3;
    string base_image = 4;
    bool thin_provisioning = 5;
    bool trim_on_shutdown = 6;
}

message DisplayConfig {
    uint32 width = 1;
    uint32 height = 2;
    uint32 dpi = 3;
    uint32 fps_limit = 4;
    DisplayEngine engine = 5;
    bool fullscreen = 6;
}

message NetworkConfig {
    NetworkMode mode = 1;
    repeated PortForward ports = 2;
    string bridge_name = 3;
}

// VM Types
message InstanceConfig {
    string id = 1;
    string name = 2;
    InstanceKind kind = 3;
    BackendKind backend = 4;
    CpuConfig cpu = 5;
    MemoryConfig memory = 6;
    DiskConfig disk = 7;
    DisplayConfig display = 8;
    RenderBackend render_backend = 9;
    NetworkConfig network = 10;
}

// Request/Response Types
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

message StatusResponse {
    bool success = 1;
    string message = 2;
    InstanceState state = 3;
}

message InstanceList {
    repeated InstanceInfo instances = 1;
}

message ResourceMetrics {
    double cpu_usage = 1;
    double memory_usage = 2;
    double disk_usage = 3;
    double network_usage = 4;
    double gpu_usage = 5;
    uint32 fps = 6;
    uint64 memory_used = 7;
    uint64 memory_total = 8;
    uint64 disk_used = 9;
    uint64 disk_total = 10;
}

message LogEntry {
    string timestamp = 1;
    string level = 2;
    string message = 3;
    string component = 4;
}

// Enumerations
enum InstanceKind {
    LINUX_VM = 0;
    ANDROID_VM = 1;
}

enum BackendKind {
    QEMU = 0;
    VMM = 1;
}

enum RenderBackend {
    VENUS = 0;
    VIRTIO_GPU = 1;
    VIRGL = 2;
    CPU = 3;
    PASSTHROUGH = 4;
}

enum NetworkMode {
    NAT = 0;
    BRIDGE = 1;
    ISOLATED = 2;
}

enum InstanceState {
    CREATED = 0;
    STARTING = 1;
    RUNNING = 2;
    PAUSED = 3;
    STOPPING = 4;
    STOPPED = 5;
    ERROR = 6;
}

enum DisplayEngine {
    SDL = 0;
    SPICE = 1;
    DBUS = 2;
    NONE = 3;
}

enum DiskFormat {
    QCOW2 = 0;
    RAW = 1;
    VDI = 2;
}

enum CpuPriority {
    LOW = 0;
    NORMAL = 1;
    HIGH = 2;
    REALTIME = 3;
}
```

## Error Handling

### Error Types

```rust
// Backend errors
pub enum BackendError {
    NotImplemented { backend: String, operation: String },
    InvalidConfig { message: String },
    ProcessFailed { message: String },
    QmpError { message: String },
    Timeout { operation: String },
    ResourceExhausted { resource: String },
}

// Disk errors
pub enum DiskError {
    QemuImgFailed { command: String, output: String },
    InvalidPath { path: String },
    InsufficientSpace { required: u64, available: u64 },
    PermissionDenied { path: String },
}

// Network errors
pub enum NetworkError {
    BridgeFailed { name: String },
    PortConflict { port: u16 },
    InterfaceNotFound { name: String },
    PermissionDenied { operation: String },
}

// General errors
pub enum AndlerError {
    ConfigError { message: String },
    StateError { message: String },
    ValidationError { message: String },
    IoError { message: String },
    NetworkError(NetworkError),
    DiskError(DiskError),
    BackendError(BackendError),
}
```

### Error Handling Strategy

1. **Fail Fast**: Validate inputs early and fail with clear error messages
2. **Graceful Degradation**: Provide fallback options when possible
3. **User-Friendly Messages**: Translate technical errors to user-friendly messages
4. **Logging**: Log all errors for debugging and monitoring
5. **Recovery**: Implement recovery mechanisms where possible

## Security Considerations

### Security Architecture

1. **Isolation**: VMs are isolated from host system and other VMs
2. **Access Control**: User permissions control VM access
3. **Data Protection**: Sensitive data is encrypted at rest
4. **Network Security**: Network access is controlled and monitored
5. **Resource Limits**: Resource usage is limited to prevent DoS

### Security Measures

#### VM Isolation
- Each VM runs in its own process space
- Network isolation between VMs
- File system isolation using overlay disks
- Memory isolation using separate memory allocations

#### Access Control
- User permissions control VM creation and management
- Configuration files are protected with proper permissions
- API access is authenticated and authorized

#### Data Protection
- VM disks can be encrypted
- Configuration files are protected
- Sensitive data is encrypted at rest

#### Network Security
- Network access is controlled per VM
- Port forwarding is explicitly configured
- Network isolation is available for sensitive VMs

#### Resource Limits
- CPU and memory limits per VM
- Disk space limits per VM
- Network bandwidth limits per VM

## Performance Considerations

### Performance Optimization

#### CPU Optimization
- CPU affinity for VMs
- CPU priority management
- Dynamic CPU allocation
- CPU pinning for performance-critical VMs

#### Memory Optimization
- Memory ballooning for dynamic allocation
- KSM for memory deduplication
- ZRAM for memory compression
- Memory limits per VM

#### Disk Optimization
- qcow2 format for space efficiency
- Thin provisioning for flexible allocation
- Automatic disk compression
- TRIM support for space reclamation

#### Network Optimization
- Network bandwidth limits
- Port forwarding optimization
- Network isolation for security
- Network monitoring for performance

### Performance Monitoring

#### Metrics Collection
- CPU usage per VM
- Memory usage per VM
- Disk I/O per VM
- Network I/O per VM
- GPU usage per VM
- FPS for graphical VMs

#### Performance Monitoring
- Real-time metrics collection
- Historical performance data
- Performance alerts and notifications
- Performance optimization recommendations

## Deployment Architecture

### Development Deployment

#### Docker Development Environment
```yaml
services:
  unit-test:
    build:
      context: ..
      dockerfile: docker/Dockerfile.dev
      target: unit-test
    # No KVM required for unit tests

  integration-test:
    build:
      context: ..
      dockerfile: docker/Dockerfile.dev
      target: integration-test
    devices:
      - /dev/kvm
    # Requires KVM for integration tests

  daemon:
    build:
      context: ..
      dockerfile: docker/Dockerfile.dev
      target: daemon
    devices:
      - /dev/kvm
    volumes:
      - andler-data:/var/lib/andler
    # For development, not production deployment

volumes:
  andler-data:
```

### Production Deployment

#### System Requirements
- Linux system with KVM support
- User in `kvm` group
- QEMU system packages
- Sufficient disk space for VMs
- Adequate memory for VMs

#### Installation Methods

1. **From Source**:
   ```bash
   git clone https://github.com/andler-project/andler.git
   cd andler
   cargo build --release
   sudo cp target/release/andlerd /usr/local/bin/
   sudo cp target/release/andler /usr/local/bin/
   ```

2. **Package Installation**:
   - Debian/Ubuntu: `.deb` package
   - Fedora/RHEL: `.rpm` package
   - Arch: AUR package
   - Cross-platform: AppImage

3. **Docker Deployment** (development only):
   ```bash
   docker compose up daemon
   ```

#### Configuration

1. **System Configuration**:
   - Add user to `kvm` group
   - Configure QEMU and KVM
   - Set up OVMF firmware
   - Configure network settings

2. **ANDLER Configuration**:
   - Configure default VM settings
   - Set up backend preferences
   - Configure storage locations
   - Set up network configuration

3. **Security Configuration**:
   - Configure user permissions
   - Set up access control
   - Configure network security
   - Set up resource limits

## Testing Strategy

### Test Types

#### Unit Tests
- Test individual components in isolation
- Test configuration validation
- Test state machine transitions
- Test backend implementations

#### Integration Tests
- Test component interactions
- Test VM lifecycle management
- Test disk operations
- Test network configuration

#### End-to-End Tests
- Test complete workflows
- Test VM creation and management
- Test resource monitoring
- Test configuration management

### Testing Infrastructure

#### Test Environment
- Docker-based test environment
- KVM for integration tests
- Mock services for unit tests
- Test fixtures for configuration

#### Test Data
- Test VM configurations
- Test disk images
- Test network configurations
- Test resource limits

#### Test Automation
- CI/CD pipeline integration
- Automated test execution
- Test result reporting
- Test coverage tracking

## Future Roadmap

### Short-term (3-6 months)
- Complete frontend implementation
- Add rust-vmm backend support
- Improve performance monitoring
- Add more network configuration options

### Medium-term (6-12 months)
- Add guest image building
- Implement preset management
- Add mobile companion app
- Improve security features

### Long-term (12+ months)
- Add Windows VM support
- Implement GPU passthrough
- Add cloud deployment options
- Improve cross-platform support

## Conclusion

ANDLER provides a comprehensive solution for managing virtual machines and Android environments on Linux systems. With its modular architecture, GPU acceleration support, and user-friendly interface, it offers a powerful platform for developers, gamers, and power users.

The project is designed to be extensible, secure, and performant, with clear separation of concerns and comprehensive testing. As the project evolves, it will continue to add new features and improve existing functionality to meet the needs of its growing user base.