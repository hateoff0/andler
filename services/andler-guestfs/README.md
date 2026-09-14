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

One appliance session per `apply` batch:
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
- Package-manager operations run in the appliance, but not as
  `virt-customize --install` installroot: the guest's *own* package manager
  runs there. `sh` (i.e. `MutatorOp::RunShell`) goes through the **guest's**
  `/bin/sh` with the guest root as `/`, as root, and the appliance mounts
  `/dev`, `/dev/pts`, `/proc` and `/sys` for it — so `apt-get`/`dnf`/`pacman`
  execute against the guest's real filesystem. `GuestfsMutator::for_packages`
  is the session shape for that work: `guestfish --network` (the appliance has
  no network of its own) plus the package-step budget. The phase-0 spike's
  "installroot does not start" result was about a package manager *inside the
  appliance*, which the Arch supermin appliance does not have; this path needs
  the guest's.
- The `guestmount` FUSE kitchen in `andler-disk` is gone: it is no longer
  needed by any path (`guest install`/`remove`, `guest list` and the
  translators all run through this crate).

## Integration notes

- Mounting uses `guestfish -i` (inspection) when the image contains a
  recognizable OS; `GuestfsMutator::with_mount(disk, "/dev/sda:/")`
  forces an explicit `-m` spec for test images.
- The conformance suite (`core::guest_mutator::conformance`) runs
  against this implementation under `cargo test -- --ignored`; it needs
  `guestfish`, `qemu-img` and `mke2fs` on PATH.

## The listening session

The appliance is the cost: a session takes about two seconds to boot before it
does anything, which is why a `guest install` that asked nine separate questions
spent ~22s of appliance time on a 21s operation whose actual work takes a
fraction of a second. `GuestfsMutator` therefore boots `guestfish --listen` once
and sends every later call to that booted session (`guestfish --remote=<pid>`),
so the same operation runs in two boots with ~10ms per call — measured,
`guest install libndk`: 20.8s → 7.7s.

Three properties the session keeps:

- it is stopped with the mutator (a kill of the serving pid, which
  `guestfish --listen` prints as `GUESTFISH_PID=`), and that line is also its
  readiness signal;
- a listener left behind by a killed daemon still holds the guest image's lock,
  so booting a session reaps orphaned listeners (a guestfish whose parent is
  gone) first;
- a command the appliance rejects can end the listener with it, and the process
  is not gone until it is reaped (a pid probe still answers), so the client
  treats "server is not running" as retryable, stops the old session before
  booting the next one, and never leaves the image lock to a session nobody owns.
