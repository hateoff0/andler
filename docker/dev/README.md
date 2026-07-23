# docker/dev — building and testing ANDLER

Building and testing ANDLER in containers — for reproducibility (same
Rust/system library version for all developers and in CI) and for isolating
tests that need `/dev/kvm` from those that don't.

## Files

- `Dockerfile.dev` — multi-stage: `builder` → `unit-test` / `integration-test` / `daemon` / `e2e`. Details and rationale for the split — comments in the file itself.
- `docker-compose.yml` — shortcuts for local runs.
- `e2e_smoke.sh` — E2E smoke test script: starts andlerd, creates an instance, checks status, logs, and metrics.

All commands below run from the **repository root** (not from `docker/dev/`) —
`docker-compose.yml` builds the image with context `../..` so it can see the
entire workspace.

## Quick start

```bash
# Build (first time or after changing dependencies — ~2-5 min)
docker compose -f docker/dev/docker-compose.yml build --no-cache unit-test

# Unit tests for the entire workspace, no /dev/kvm — works everywhere, including
# CI runners without virtualization.
docker compose -f docker/dev/docker-compose.yml run --rm unit-test

# Integration tests (#[ignore] tests: qemu-img, QMP round-trip).
# Requires /dev/kvm on the host.
docker compose -f docker/dev/docker-compose.yml build --no-cache integration-test
docker compose -f docker/dev/docker-compose.yml run --rm integration-test

# E2E smoke test: real andlerd + andler, TCP, sqlite.
# Requires /dev/kvm on the host.
docker compose -f docker/dev/docker-compose.yml build --no-cache e2e
docker compose -f docker/dev/docker-compose.yml run --rm e2e
```

### Without `--no-cache`

If dependencies haven't changed, cached builds take ~10 seconds.
`--no-cache` is needed on the first run or after changing `Cargo.toml`/`Cargo.lock`.

## What runs in each target

| Target | What it does | Needs `/dev/kvm` | Needs `qemu-img` |
|--------|-------------|:-----------------:|:-----------------:|
| `unit-test` | `cargo test --workspace` — all tests without `#[ignore]` | No | No |
| `integration-test` | `cargo test --workspace -- --ignored` — #[ignore] tests (qemu-img, QMP, spawn) | Yes | Yes |
| `e2e` | `e2e_smoke.sh` — real andlerd + andler, TCP, sqlite, restart | Yes | Yes |
| `daemon` | Minimal runtime image for andlerd (not test-oriented) | Yes | Yes |

## Why not one target for everything

`andler-core` is a pure domain crate and tests without any environment.
Some `andler-qemu`/`andler-daemon` tests require a real QEMU process and
`/dev/kvm`. Mixing everything into one `cargo test --workspace` without
splitting targets would mean CI on every PR either drags in the KVM dependency
where it isn't needed or silently skips integration tests. The split follows
§7 of the architecture plan: fast `cargo test --workspace` on every PR,
separate job with `/dev/kvm` on merge to the main branch.

## Building all targets

```bash
# Build all images at once (unit-test + integration-test + daemon + e2e)
docker compose -f docker/dev/docker-compose.yml build --no-cache
```

## Convention for tests requiring external binaries

A test that needs real `/dev/kvm` (andler-qemu) or just the `qemu-img`
binary without KVM (andler-disk) is marked `#[ignore]` with a comment
explaining the reason (not just bare `#[ignore]` — otherwise it's unclear
whether the test is temporarily disabled or fundamentally requires a specific
environment). The `integration-test` target runs such tests explicitly via
`cargo test -- --ignored` (it has both `qemu-utils` and `/dev/kvm`). The
`e2e` target goes further — it starts a real andlerd and runs
`e2e_smoke.sh`.

## Production deployment is NOT Docker

The `daemon` target exists for development/CI, not as a delivery mechanism
for `andlerd` to end users. The final distribution is `.deb`/`.rpm`/AUR/AppImage
(see `packaging/`), because the GUI client and daemon on the user's machine
need direct access to `/dev/kvm` and the host display, which doesn't play
well with typical container isolation.

## Troubleshooting

### KVM: permission denied

```bash
# Check /dev/kvm availability
ls -la /dev/kvm

# Add the user to the kvm group
sudo usermod -aG kvm $USER
# Re-login for the change to take effect
```

### Build fails with a protobuf error

```bash
# Make sure protoc is installed (required for andler-rpc)
which protoc || sudo apt install protobuf-compiler
```

### Clearing Docker cache

```bash
docker system prune -f
docker compose -f docker/dev/docker-compose.yml build --no-cache unit-test
```
