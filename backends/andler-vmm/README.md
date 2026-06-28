# andler-vmm

**Empty stub.** Backend template for a future `rust-vmm` / Cloud Hypervisor integration — not used until the open question "when is rust-vmm mature enough" is resolved (see project architecture docs).

## Current State

`impl HypervisorBackend for VmmBackend` exists and is registered in the daemon, but every method returns `Err(BackendError::NotImplemented { backend: "vmm", operation: ... })`. Not `todo!()`/`unimplemented!()` — the daemon must not panic if a user explicitly selects this backend before it's ready.

Do not add real logic here until a separate decision is made to start this phase.
