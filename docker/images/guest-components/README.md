# Guest components

Declarative guest provisioning units, applied with
`andler guest provision <component>/manifest.toml <instance-id>`:

| Component | What it installs | Notes |
|-----------|------------------|-------|
| `spice-agent/` | Example manifest (mkdir/write/upload/symlink ops) | Shape reference; real SPICE agent packaging lands with the input-component work (§7) |

Every manifest is a plain TOML with `schema_version = 1`, a `name`, and
an `[[ops]]` list — see `docs/API.md` (Provision manifests) for the full
op reference. Relative `host_path` values are resolved against the
manifest's own directory, so a component ships its payload files next to
`manifest.toml`.
