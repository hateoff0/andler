# andler-guestfs

Offline guest-filesystem mutation through the libguestfs appliance.

## What this crate is

`GuestfsMutator` — one of the two real implementations of
`andler_core::GuestMutator` (the other is `QgaMutator` in
`backends/andler-qemu`). It drives `guestfish` against a qcow2/raw
image: the appliance boots its own unprivileged QEMU, mounts the guest
filesystem with an exclusive qemu image lock, and runs the mutation
batch. Zero root on the host — no nbd, no mount, no helper.

## Session model

One appliance session per `apply` batch (per §6 of the refactor plan):
a staging run that touches hundreds of files is a single `guestfish`
invocation, not one per file. `WriteFile` content is staged into a
host temp dir and uploaded with `upload`, keeping binary content out
of the script.

## Crate boundaries

- `MutatorOp` / `GuestMutator` / `MutatorError` / the shared
  conformance suite live in `andler-core` (`guest_mutator.rs`) — no I/O.
- `GuestfsMutator` lives here, not in `andler-disk`: the appliance has
  a different lifecycle (external process, appliance boot, batch
  sessions) and a different dependency set than qcow2/DiskChain code.
- Package-manager operations (installroot) do **not** work in the
  appliance — the phase-0 spike confirmed the suspended variant; guest
  package install/remove still runs on the chroot kitchen in
  `andler-disk` through `andler-helper`.

## Integration notes

- Mounting uses `guestfish -i` (inspection) when the image contains a
  recognizable OS; `GuestfsMutator::with_mount(disk, "/dev/sda:/")`
  forces an explicit `-m` spec for test images.
- The conformance suite (`core::guest_mutator::conformance`) runs
  against this implementation under `cargo test -- --ignored`; it needs
  `guestfish`, `qemu-img` and `mke2fs` on PATH.
