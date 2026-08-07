# syntax=docker/dockerfile:1

# docker/e2e/Dockerfile — containerized build & test harness for ANDLER.
#
# Stages:
#   builder           compiles the workspace; BuildKit cache mounts make
#                     source-only rebuilds incremental (see below); dist/
#                     carries the two binaries into the runtime stages
#   testbuild         builder + a baked copy of the cargo cache, so the test
#                     stages run `cargo test` without rebuilding or network
#   unit-test         cargo test --workspace                (no KVM needed)
#   integration-test  cargo test --workspace -- --ignored   (needs /dev/kvm)
#   e2e               slim runtime: andlerd + andler + qemu + ovmf + the suite
#   daemon            slim runtime image for andlerd (manual use)
#
# Speed: the registry and target directories are BuildKit cache mounts
# (`--mount=type=cache`), which are NOT part of the image. After a source-only
# change, `docker compose build e2e` recompiles just the affected crates
# instead of the whole workspace. `testbuild` bakes the cache into its image so
# `docker compose run unit-test` needs no rebuild and no network.
#
# Cache invalidation after a rust toolchain bump: `docker builder prune
# --filter type=exec.cachemount`.

ARG RUST_VERSION=1

# --- builder: compiles everything -------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS builder

WORKDIR /andler

RUN apt-get update && apt-get install -y --no-install-recommends \
        protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY core/andler-core/Cargo.toml core/andler-core/Cargo.toml
COPY backends/andler-qemu/Cargo.toml backends/andler-qemu/Cargo.toml
COPY services/andler-disk/Cargo.toml services/andler-disk/Cargo.toml
COPY services/andler-net/Cargo.toml services/andler-net/Cargo.toml
COPY services/andler-firmware/Cargo.toml services/andler-firmware/Cargo.toml
COPY services/andler-store/Cargo.toml services/andler-store/Cargo.toml
COPY services/andler-rpc/Cargo.toml services/andler-rpc/Cargo.toml
COPY apps/daemon/Cargo.toml apps/daemon/Cargo.toml
COPY apps/cli/Cargo.toml apps/cli/Cargo.toml
COPY apps/helper/Cargo.toml apps/helper/Cargo.toml

# Stub sources so `cargo fetch` can resolve the workspace graph
RUN for crate in andler-core andler-qemu andler-disk andler-net andler-firmware andler-store andler-rpc; do \
        dir="core/$crate"; [ -d "backends/$crate" ] && dir="backends/$crate"; [ -d "services/$crate" ] && dir="services/$crate"; \
        mkdir -p "$dir/src" && echo "// placeholder" > "$dir/src/lib.rs"; \
    done && \
    mkdir -p apps/daemon/src && echo "fn main() {}" > apps/daemon/src/main.rs && \
    mkdir -p apps/cli/src && echo "fn main() {}" > apps/cli/src/main.rs && \
    mkdir -p apps/helper/src && echo "fn main() {}" > apps/helper/src/main.rs

# Dependency downloads only happen on the first build; afterwards the registry
# cache mount survives layer invalidation.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo fetch

COPY core/ core/
COPY backends/ backends/
COPY services/ services/
COPY apps/ apps/

# --bins required alongside --tests (cargo build narrows to specified target kind)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/andler/target \
    cargo build --workspace --bins --tests \
    && mkdir -p /andler/dist \
    && cp target/debug/andler target/debug/andlerd target/debug/andler-helper /andler/dist/

# --- testbuild: bake the cargo cache into the image -------------------------
FROM builder AS testbuild

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/andler/target \
    mkdir -p /opt/e2e-bake \
    && cp -a /usr/local/cargo/registry /usr/local/cargo/git /opt/e2e-bake/ \
    && cp -a /andler/target /opt/e2e-bake/target

# --- unit-test: full workspace unit tests, no KVM ---------------------------
FROM testbuild AS unit-test

RUN rm -rf /usr/local/cargo/registry /usr/local/cargo/git /andler/target \
    && cp -a /opt/e2e-bake/registry /opt/e2e-bake/git /usr/local/cargo/ \
    && cp -a /opt/e2e-bake/target /andler/ \
    && rm -rf /opt/e2e-bake

CMD ["cargo", "test", "--workspace", "--offline"]

# --- integration-test: #[ignore] tests (qemu-img, QMP, spawn) ---------------
FROM unit-test AS integration-test

RUN apt-get update && apt-get install -y --no-install-recommends \
        qemu-system-x86 \
        qemu-utils \
    && rm -rf /var/lib/apt/lists/*

CMD ["cargo", "test", "--workspace", "--offline", "--", "--ignored"]

# --- e2e: slim runtime with the full test suite -----------------------------
FROM debian:bookworm-slim AS e2e

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        debootstrap \
        e2fsprogs \
        fdisk \
        jq \
        kmod \
        ovmf \
        procps \
        qemu-system-x86 \
        qemu-utils \
        sudo \
        util-linux \
    && rm -rf /var/lib/apt/lists/*

# Map Arch-style OVMF path (what andler-firmware probes) to the Debian package
RUN mkdir -p /usr/share/edk2-ovmf/x64 && \
    CODE_SRC="$(ls /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd 2>/dev/null | head -n1)" && \
    test -n "$CODE_SRC" && \
    cp "$CODE_SRC" /usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd

# Minimal Debian rootfs for the deep guest tests (offline package
# install/remove/list and Android boot-mode switching run against a real
# filesystem through qemu-nbd). resolv.conf is refreshed at test time from the
# container's own /etc/resolv.conf.
RUN debootstrap --variant=minbase --include=systemd bookworm /opt/e2e/guest-rootfs \
        http://deb.debian.org/debian \
    && rm -f /opt/e2e/guest-rootfs/etc/resolv.conf \
    && echo "nameserver 8.8.8.8" > /opt/e2e/guest-rootfs/etc/resolv.conf \
    && touch /opt/e2e/guest-rootfs/etc/systemd/system/android.target \
    && rm -rf /opt/e2e/guest-rootfs/var/lib/apt/lists/*

COPY --from=builder /andler/dist/andler /usr/local/bin/andler
COPY --from=builder /andler/dist/andlerd /usr/local/bin/andlerd
COPY --from=builder /andler/dist/andler-helper /usr/local/sbin/andler-helper
COPY docker/e2e/e2e.sh /usr/local/bin/andler-e2e
COPY docker/e2e/tests /usr/local/share/andler-e2e/tests

RUN chmod +x /usr/local/bin/andler-e2e /usr/local/bin/andler /usr/local/bin/andlerd /usr/local/sbin/andler-helper

CMD ["/usr/local/bin/andler-e2e"]

# --- daemon: minimal runtime image for andlerd (manual use) -----------------
FROM debian:bookworm-slim AS daemon

RUN apt-get update && apt-get install -y --no-install-recommends \
        qemu-system-x86 \
        qemu-utils \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /andler/dist/andlerd /usr/local/bin/andlerd

ENTRYPOINT ["/usr/local/bin/andlerd"]
