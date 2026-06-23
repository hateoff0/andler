# API Reference

## Overview

ANDLER provides a comprehensive API for managing virtual machines and Android environments. The API is available through multiple interfaces:

1. **gRPC API** - Primary API for programmatic access
2. **CLI Interface** - Command-line interface for direct usage
3. **REST API** - Future REST-based API for web integration

## gRPC API

### Service Interface

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

### Request/Response Types

#### VM Configuration Types

```protobuf
// CPU Configuration
message CpuConfig {
    uint32 cores = 1;           // Number of CPU cores
    uint32 sockets = 2;         // Number of CPU sockets
    uint32 threads = 3;         // Number of threads per core
    repeated uint32 affinity = 4; // CPU affinity mask
    CpuPriority priority = 5;   // CPU priority level
}

// Memory Configuration
message MemoryConfig {
    uint64 size = 1;            // Memory size in bytes
    bool ballooning = 2;        // Enable memory ballooning
    bool zram = 3;              // Enable ZRAM compression
    bool ksm = 4;               // Enable Kernel Samepage Merging
}

// Disk Configuration
message DiskConfig {
    string path = 1;            // Disk image path
    uint64 size = 2;            // Disk size in bytes
    DiskFormat format = 3;      // Disk format (qcow2, raw, vdi)
    string base_image = 4;      // Base image path for overlay disks
    bool thin_provisioning = 5; // Enable thin provisioning
    bool trim_on_shutdown = 6;  // Enable TRIM on shutdown
}

// Display Configuration
message DisplayConfig {
    uint32 width = 1;           // Display width
    uint32 height = 2;          // Display height
    uint32 dpi = 3;             // Display DPI
    uint32 fps_limit = 4;       // FPS limit
    DisplayEngine engine = 5;   // Display engine
    bool fullscreen = 6;        // Fullscreen mode
}

// Network Configuration
message NetworkConfig {
    NetworkMode mode = 1;       // Network mode
    repeated PortForward ports = 2; // Port forwarding rules
    string bridge_name = 3;     // Bridge network name
}

// VM Configuration
message InstanceConfig {
    string id = 1;              // Instance ID
    string name = 2;            // Instance name
    InstanceKind kind = 3;      // Instance type
    BackendKind backend = 4;    // Backend type
    CpuConfig cpu = 5;          // CPU configuration
    MemoryConfig memory = 6;    // Memory configuration
    DiskConfig disk = 7;        // Disk configuration
    DisplayConfig display = 8;  // Display configuration
    RenderBackend render_backend = 9; // Render backend
    NetworkConfig network = 10; // Network configuration
}
```

#### Request/Response Types

```protobuf
// Create Instance Request
message CreateInstanceRequest {
    string name = 1;            // Instance name
    InstanceKind kind = 2;      // Instance type
    BackendKind backend = 3;    // Backend type
    CpuConfig cpu = 4;          // CPU configuration
    MemoryConfig memory = 5;    // Memory configuration
    DiskConfig disk = 6;        // Disk configuration
    DisplayConfig display = 7;  // Display configuration
    RenderBackend render_backend = 8; // Render backend
    NetworkConfig network = 9;  // Network configuration
}

// Start Instance Request
message StartInstanceRequest {
    string instance_id = 1;     // Instance ID
}

// Stop Instance Request
message StopInstanceRequest {
    string instance_id = 1;     // Instance ID
    bool graceful = 2;          // Graceful shutdown
}

// Pause Instance Request
message PauseInstanceRequest {
    string instance_id = 1;     // Instance ID
}

// Resume Instance Request
message ResumeInstanceRequest {
    string instance_id = 1;     // Instance ID
}

// Clone Instance Request
message CloneInstanceRequest {
    string source_id = 1;       // Source instance ID
    string new_name = 2;        // New instance name
    bool linked_clone = 3;      // Use linked clone
}

// Remove Instance Request
message RemoveInstanceRequest {
    string instance_id = 1;     // Instance ID
}

// List Instances Request
message ListInstancesRequest {
    string filter = 1;          // Filter by name or type
    bool include_stopped = 2;   // Include stopped instances
}

// Get Instance Status Request
message GetInstanceStatusRequest {
    string instance_id = 1;     // Instance ID
}

// Stream Instance Logs Request
message StreamInstanceLogsRequest {
    string instance_id = 1;     // Instance ID
    uint32 tail_lines = 2;      // Number of recent lines
    bool follow = 3;            // Follow log stream
}

// Stream Resource Metrics Request
message StreamResourceMetricsRequest {
    string instance_id = 1;     // Instance ID
    uint32 interval_ms = 2;     // Update interval in milliseconds
}

// Instance Information
message InstanceInfo {
    string id = 1;              // Instance ID
    string name = 2;            // Instance name
    InstanceState state = 3;    // Instance state
    ResourceMetrics metrics = 4; // Resource metrics
    repeated string tags = 5;   // Instance tags
}

// Status Response
message StatusResponse {
    bool success = 1;           // Operation success
    string message = 2;         // Status message
    InstanceState state = 3;    // Instance state
}

// Instance List
message InstanceList {
    repeated InstanceInfo instances = 1; // List of instances
}

// Resource Metrics
message ResourceMetrics {
    double cpu_usage = 1;       // CPU usage percentage
    double memory_usage = 2;    // Memory usage percentage
    double disk_usage = 3;      // Disk usage percentage
    double network_usage = 4;   // Network usage percentage
    double gpu_usage = 5;       // GPU usage percentage
    uint32 fps = 6;             // Frames per second
    uint64 memory_used = 7;     // Memory used in bytes
    uint64 memory_total = 8;    // Total memory in bytes
    uint64 disk_used = 9;       // Disk used in bytes
    uint64 disk_total = 10;     // Total disk in bytes
}

// Log Entry
message LogEntry {
    string timestamp = 1;       // Log timestamp
    string level = 2;           // Log level
    string message = 3;         // Log message
    string component = 4;       // Log component
}
```

#### Enumerations

```protobuf
// Instance Types
enum InstanceKind {
    LINUX_VM = 0;               // Linux virtual machine
    ANDROID_VM = 1;             // Android virtual machine
}

// Backend Types
enum BackendKind {
    QEMU = 0;                   // QEMU backend
    VMM = 1;                    // rust-vmm backend
}

// Render Backends
enum RenderBackend {
    VENUS = 0;                  // Vulkan proxy (maximum performance)
    VIRTIO_GPU = 1;             // Virtual GPU (universal)
    VIRGL = 2;                  // OpenGL ES proxy
    CPU = 3;                    // Software rendering
    PASSTHROUGH = 4;            // GPU passthrough
}

// Network Modes
enum NetworkMode {
    NAT = 0;                    // Network Address Translation
    BRIDGE = 1;                 // Bridge to host network
    ISOLATED = 2;               // No network access
}

// Instance States
enum InstanceState {
    CREATED = 0;                // Instance created
    STARTING = 1;               // Instance starting
    RUNNING = 2;                // Instance running
    PAUSED = 3;                 // Instance paused
    STOPPING = 4;               // Instance stopping
    STOPPED = 5;                // Instance stopped
    ERROR = 6;                  // Instance error
}

// Display Engines
enum DisplayEngine {
    SDL = 0;                    // SDL display engine
    SPICE = 1;                  // SPICE display engine
    DBUS = 2;                   // D-Bus display engine
    NONE = 3;                   // No display
}

// Disk Formats
enum DiskFormat {
    QCOW2 = 0;                  // qcow2 format
    RAW = 1;                    // Raw format
    VDI = 2;                    // VDI format
}

// CPU Priorities
enum CpuPriority {
    LOW = 0;                    // Low priority
    NORMAL = 1;                 // Normal priority
    HIGH = 2;                   // High priority
    REALTIME = 3;               // Real-time priority
}
```

#### Port Forwarding

```protobuf
// Port Forwarding Rule
message PortForward {
    uint32 host_port = 1;       // Host port
    uint32 guest_port = 2;      // Guest port
    string protocol = 3;        // Protocol (tcp/udp)
}
```

## CLI Interface

### Command Structure

```bash
# Basic command structure
andler <command> [options] [arguments]

# Common options
--help, -h    # Show help
--verbose, -v # Verbose output
--quiet, -q   # Quiet output
--json, -j    # JSON output
```

### VM Management Commands

#### Create Instance

```bash
# Create a Linux VM
andler create --name my-linux-vm --type linux --iso /path/to/iso

# Create an Android VM
andler create --name my-android-vm --type android

# Create a VM with specific configuration
andler create --name my-vm --type linux --iso /path/to/iso \
    --cpu 4 --memory 8G --disk 40G \
    --render-backend venus --network nat
```

**Options:**
- `--name, -n`: Instance name (required)
- `--type, -t`: Instance type (linux/android, required)
- `--iso`: ISO image path (for Linux VMs)
- `--backend, -b`: Backend type (qemu/vmm)
- `--cpu, -c`: Number of CPU cores
- `--memory, -m`: Memory size (e.g., 4G, 8G)
- `--disk, -d`: Disk size (e.g., 20G, 40G)
- `--render-backend, -r`: Render backend (venus/virtio-gpu/virgl/cpu)
- `--network, -n`: Network mode (nat/bridge/isolated)
- `--tags, -g`: Comma-separated tags

#### Start Instance

```bash
# Start an instance
andler start my-vm

# Start with verbose output
andler start --verbose my-vm
```

**Options:**
- `--verbose, -v`: Verbose output
- `--background, -b`: Start in background

#### Stop Instance

```bash
# Stop an instance
andler stop my-vm

# Graceful stop
andler stop --graceful my-vm
```

**Options:**
- `--graceful, -g`: Graceful shutdown
- `--force, -f`: Force stop

#### Pause Instance

```bash
# Pause an instance
andler pause my-vm
```

#### Resume Instance

```bash
# Resume a paused instance
andler resume my-vm
```

#### Clone Instance

```bash
# Clone an instance
andler clone --name new-vm my-vm

# Create a linked clone
andler clone --name new-vm --linked my-vm
```

**Options:**
- `--name, -n`: New instance name (required)
- `--linked, -l`: Create linked clone

#### Remove Instance

```bash
# Remove an instance
andler remove my-vm

# Force remove
andler remove --force my-vm
```

**Options:**
- `--force, -f`: Force removal
- `--keep-disk, -k`: Keep disk image

#### List Instances

```bash
# List all instances
andler list

# List running instances only
andler list --running

# List with detailed information
andler list --verbose

# List with JSON output
andler list --json
```

**Options:**
- `--running, -r`: List only running instances
- `--verbose, -v`: Detailed information
- `--json, -j`: JSON output
- `--filter, -f`: Filter by name or type

#### Get Instance Status

```bash
# Get instance status
andler status my-vm

# Get detailed status
andler status --verbose my-vm

# Get status in JSON format
andler status --json my-vm
```

**Options:**
- `--verbose, -v`: Detailed information
- `--json, -j`: JSON output

### Configuration Commands

#### Get Configuration

```bash
# Get current configuration
andler config --get

# Get specific configuration value
andler config --get key=value

# Get configuration in JSON format
andler config --get --json
```

**Options:**
- `--get, -g`: Get configuration
- `--set, -s`: Set configuration
- `--json, -j`: JSON output
- `--key, -k`: Configuration key

#### Set Configuration

```bash
# Set configuration value
andler config --set key=value

# Set multiple configuration values
andler config --set cpu=4 memory=8G disk=40G

# Set configuration in JSON format
andler config --set --json '{"cpu": 4, "memory": "8G"}'
```

**Options:**
- `--set, -s`: Set configuration
- `--json, -j`: JSON input
- `--key, -k`: Configuration key

### Snapshot Commands

#### Create Snapshot

```bash
# Create a snapshot
andler snapshot --create --tag backup my-vm

# Create snapshot with description
andler snapshot --create --tag backup --description "Before update" my-vm
```

**Options:**
- `--create, -c`: Create snapshot
- `--tag, -t`: Snapshot tag (required)
- `--description, -d`: Snapshot description

#### Restore Snapshot

```bash
# Restore a snapshot
andler snapshot --restore --tag backup my-vm

# Restore with confirmation
andler snapshot --restore --tag backup --confirm my-vm
```

**Options:**
- `--restore, -r`: Restore snapshot
- `--tag, -t`: Snapshot tag (required)
- `--confirm, -y`: Confirm restoration

#### List Snapshots

```bash
# List snapshots for an instance
andler snapshot --list my-vm

# List snapshots with detailed information
andler snapshot --list --verbose my-vm

# List snapshots in JSON format
andler snapshot --list --json my-vm
```

**Options:**
- `--list, -l`: List snapshots
- `--verbose, -v`: Detailed information
- `--json, -j`: JSON output

### Resource Monitoring Commands

#### Stream Metrics

```bash
# Stream resource metrics
andler metrics --stream my-vm

# Stream metrics with custom interval
andler metrics --stream --interval 5000 my-vm

# Stream metrics in JSON format
andler metrics --stream --json my-vm
```

**Options:**
- `--stream, -s`: Stream metrics
- `--interval, -i`: Update interval (milliseconds)
- `--json, -j`: JSON output

#### Get Current Metrics

```bash
# Get current metrics
andler metrics my-vm

# Get current metrics with detailed information
andler metrics --verbose my-vm

# Get current metrics in JSON format
andler metrics --json my-vm
```

**Options:**
- `--verbose, -v`: Detailed information
- `--json, -j`: JSON output

#### Stream Logs

```bash
# Stream logs
andler logs --stream my-vm

# Stream logs with tail
andler logs --stream --tail 100 my-vm

# Stream logs in JSON format
andler logs --stream --json my-vm
```

**Options:**
- `--stream, -s`: Stream logs
- `--tail, -t`: Number of recent lines
- `--json, -j`: JSON output
- `--follow, -f`: Follow log stream

## REST API (Future)

### Overview

The REST API will provide a web-based interface for managing ANDLER instances. It will be available through a web-based dashboard and can be integrated with other web applications.

### API Endpoints

#### VM Management

```http
# Create a VM
POST /api/v1/instances
Content-Type: application/json
{
    "name": "my-vm",
    "type": "linux",
    "iso": "/path/to/iso",
    "cpu": 4,
    "memory": "8G",
    "disk": "40G"
}

# Start a VM
POST /api/v1/instances/{id}/start

# Stop a VM
POST /api/v1/instances/{id}/stop

# Pause a VM
POST /api/v1/instances/{id}/pause

# Resume a VM
POST /api/v1/instances/{id}/resume

# Clone a VM
POST /api/v1/instances/{id}/clone
Content-Type: application/json
{
    "name": "new-vm",
    "linked": false
}

# Remove a VM
DELETE /api/v1/instances/{id}
```

#### VM Information

```http
# List all VMs
GET /api/v1/instances

# Get VM status
GET /api/v1/instances/{id}

# Get VM metrics
GET /api/v1/instances/{id}/metrics

# Get VM logs
GET /api/v1/instances/{id}/logs
```

#### VM Snapshots

```http
# Create a snapshot
POST /api/v1/instances/{id}/snapshots
Content-Type: application/json
{
    "tag": "backup",
    "description": "Before update"
}

# Restore a snapshot
POST /api/v1/instances/{id}/snapshots/{tag}/restore

# List snapshots
GET /api/v1/instances/{id}/snapshots
```

### Authentication

The REST API will support multiple authentication methods:

1. **API Keys**: Simple API key authentication
2. **OAuth 2.0**: For web-based applications
3. **JWT**: For token-based authentication

### WebSocket Support

The REST API will support WebSocket connections for real-time updates:

```javascript
// Connect to WebSocket
const ws = new WebSocket('ws://localhost:8080/ws');

// Listen for updates
ws.onmessage = (event) => {
    const data = JSON.parse(event.data);
    console.log('Received update:', data);
};

// Subscribe to VM updates
ws.send(JSON.stringify({
    type: 'subscribe',
    instance_id: 'my-vm',
    events: ['status', 'metrics', 'logs']
}));
```

## Error Handling

### Error Response Format

```json
{
    "error": {
        "code": "INVALID_CONFIG",
        "message": "Invalid configuration: CPU cores must be between 1 and 16",
        "details": {
            "field": "cpu",
            "value": 32,
            "min": 1,
            "max": 16
        },
        "timestamp": "2024-01-01T12:00:00Z"
    }
}
```

### Error Codes

| Error Code | Description | HTTP Status |
|------------|-------------|-------------|
| INVALID_CONFIG | Invalid configuration | 400 |
| INSTANCE_NOT_FOUND | Instance not found | 404 |
| INSTANCE_ALREADY_EXISTS | Instance already exists | 409 |
| INSTANCE_NOT_RUNNING | Instance not running | 400 |
| INSTANCE_NOT_STOPPED | Instance not stopped | 400 |
| INSUFFICIENT_RESOURCES | Insufficient resources | 429 |
| PERMISSION_DENIED | Permission denied | 403 |
| INTERNAL_ERROR | Internal error | 500 |
| SERVICE_UNAVAILABLE | Service unavailable | 503 |

### Error Handling Examples

```bash
# Error handling with CLI
andler create --name my-vm --type linux --iso /path/to/iso
# Error: INVALID_CONFIG - ISO file not found

# Error handling with gRPC
try {
    const response = await client.CreateInstance(request);
} catch (error) {
    if (error.code === 'INVALID_CONFIG') {
        console.error('Invalid configuration:', error.details);
    } else if (error.code === 'INSTANCE_NOT_FOUND') {
        console.error('Instance not found:', error.details);
    }
}
```

## SDK Support

### Python SDK

```python
import andler

# Create client
client = andler.Client('localhost:50051')

# Create VM
instance = client.create_instance(
    name='my-vm',
    type='linux',
    iso='/path/to/iso',
    cpu=4,
    memory='8G',
    disk='40G'
)

# Start VM
client.start_instance(instance.id)

# Get VM status
status = client.get_instance_status(instance.id)
print(f"VM status: {status.state}")

# List VMs
instances = client.list_instances()
for vm in instances:
    print(f"{vm.name}: {vm.state}")
```

### JavaScript SDK

```javascript
import { AndlerClient } from '@andler/sdk';

// Create client
const client = new AndlerClient('localhost:50051');

// Create VM
const instance = await client.createInstance({
    name: 'my-vm',
    type: 'linux',
    iso: '/path/to/iso',
    cpu: 4,
    memory: '8G',
    disk: '40G'
});

// Start VM
await client.startInstance(instance.id);

// Get VM status
const status = await client.getInstanceStatus(instance.id);
console.log(`VM status: ${status.state}`);

// List VMs
const instances = await client.listInstances();
instances.forEach(vm => {
    console.log(`${vm.name}: ${vm.state}`);
});
```

### Go SDK

```go
import "github.com/andler/sdk/go"

// Create client
client := andler.NewClient("localhost:50051")

// Create VM
instance, err := client.CreateInstance(&andler.CreateInstanceRequest{
    Name: "my-vm",
    Type: andler.InstanceKind_LINUX_VM,
    Iso:  "/path/to/iso",
    Cpu:  4,
    Memory: "8G",
    Disk: "40G",
})
if err != nil {
    log.Fatal(err)
}

// Start VM
_, err = client.StartInstance(&andler.StartInstanceRequest{
    InstanceId: instance.Id,
})
if err != nil {
    log.Fatal(err)
}

// Get VM status
status, err := client.GetInstanceStatus(&andler.GetInstanceStatusRequest{
    InstanceId: instance.Id,
})
if err != nil {
    log.Fatal(err)
}
fmt.Printf("VM status: %v\n", status.State)

// List VMs
instances, err := client.ListInstances(&andler.ListInstancesRequest{})
if err != nil {
    log.Fatal(err)
}
for _, vm := range instances.Instances {
    fmt.Printf("%s: %v\n", vm.Name, vm.State)
}
```

## API Versioning

### Version Strategy

ANDLER uses semantic versioning for its API:

- **Major version**: Breaking changes
- **Minor version**: New features, backward compatible
- **Patch version**: Bug fixes, backward compatible

### Version Headers

```http
# Request
GET /api/v1/instances
X-Andler-API-Version: 1.0.0

# Response
HTTP/1.1 200 OK
X-Andler-API-Version: 1.0.0
```

### Deprecation Policy

1. **Deprecation Notice**: 6 months notice before removal
2. **Migration Guide**: Provided for deprecated features
3. **Backward Compatibility**: Maintained for deprecated features during notice period

### Version Compatibility

| API Version | Status | End of Life |
|-------------|--------|-------------|
| v1.0.0 | Current | - |
| v0.9.0 | Deprecated | 2024-06-01 |
| v0.8.0 | Deprecated | 2024-03-01 |

## API Documentation

### OpenAPI Specification

The API documentation is available in OpenAPI format:

```yaml
openapi: 3.0.3
info:
  title: ANDLER API
  version: 1.0.0
  description: API for managing virtual machines and Android environments
servers:
  - url: http://localhost:50051
    description: Local development server
paths:
  /api/v1/instances:
    post:
      summary: Create a new instance
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateInstanceRequest'
      responses:
        '200':
          description: Instance created successfully
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/InstanceInfo'
```

### API Explorer

An interactive API explorer is available at:
- **Development**: `http://localhost:50051/api/explorer`
- **Production**: `https://api.andler.dev/explorer`

### API Examples

#### Example 1: Create and Start a VM

```bash
# Create a VM
curl -X POST http://localhost:50051/api/v1/instances \
  -H "Content-Type: application/json" \
  -d '{
    "name": "my-vm",
    "type": "linux",
    "iso": "/path/to/iso",
    "cpu": 4,
    "memory": "8G",
    "disk": "40G"
  }'

# Start the VM
curl -X POST http://localhost:50051/api/v1/instances/my-vm/start
```

#### Example 2: Monitor VM Resources

```bash
# Stream VM metrics
curl -N http://localhost:50051/api/v1/instances/my-vm/metrics/stream
```

#### Example 3: Manage Snapshots

```bash
# Create a snapshot
curl -X POST http://localhost:50051/api/v1/instances/my-vm/snapshots \
  -H "Content-Type: application/json" \
  -d '{
    "tag": "backup",
    "description": "Before update"
  }'

# Restore a snapshot
curl -X POST http://localhost:50051/api/v1/instances/my-vm/snapshots/backup/restore
```

## API Security

### Authentication

1. **API Keys**: Generate API keys through the dashboard
2. **OAuth 2.0**: Configure OAuth providers for web applications
3. **JWT**: Use JWT tokens for token-based authentication

### Authorization

1. **Role-Based Access Control**: Define roles and permissions
2. **Resource-Level Access**: Control access to specific resources
3. **API Key Scopes**: Limit API key permissions

### Security Best Practices

1. **Use HTTPS**: Always use HTTPS for API communication
2. **Rotate API Keys**: Regularly rotate API keys
3. **Limit API Access**: Use IP whitelisting for API access
4. **Monitor API Usage**: Monitor API usage for anomalies
5. **Rate Limiting**: Implement rate limiting for API endpoints

## API Limits

### Rate Limits

| Endpoint | Rate Limit | Window |
|----------|------------|--------|
| All endpoints | 100 requests | 1 minute |
| Create Instance | 10 requests | 1 minute |
| Start/Stop Instance | 20 requests | 1 minute |
| Stream Metrics | 5 requests | 1 minute |

### Resource Limits

| Resource | Limit |
|----------|-------|
| Maximum VMs per user | 100 |
| Maximum CPU cores per VM | 16 |
| Maximum memory per VM | 64GB |
| Maximum disk size per VM | 1TB |
| Maximum network interfaces per VM | 4 |

### Timeout Limits

| Operation | Timeout |
|-----------|---------|
| VM Creation | 5 minutes |
| VM Start/Stop | 2 minutes |
| VM Clone | 10 minutes |
| Snapshot Creation | 5 minutes |
| Snapshot Restore | 2 minutes |

## API Changelog

### v1.0.0 (Current)

- Initial API release
- gRPC API for VM management
- CLI interface for direct usage
- REST API for web integration
- WebSocket support for real-time updates
- SDK support for Python, JavaScript, and Go

### v0.9.0 (Deprecated)

- Basic VM management
- Limited API functionality
- No WebSocket support
- No SDK support

### v0.8.0 (Deprecated)

- Initial API prototype
- Basic REST endpoints
- No authentication
- No rate limiting