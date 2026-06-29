# CLI Decomposition — Complete

## Result

`cli/src/main.rs` decomposed from 1185 lines into 8 modules.

## Final Structure

```
cli/src/
├── main.rs                ← Cli struct, Command enum, dispatch (431 lines)
├── instance_file.rs       ← TOML parser (452 lines, untouched)
├── create.rs              ← Create command + build_*_request (162 lines)
├── snapshot.rs            ← SnapshotAction dispatch (75 lines)
├── disk.rs                ← DiskAction dispatch (44 lines)
├── status.rs              ← Status, List, Config, Logs, Metrics (293 lines)
├── lifecycle.rs           ← Start, Stop, Pause, Resume, Remove (66 lines)
├── clone.rs               ← Clone, Export (41 lines)
└── helpers.rs             ← parse_size, format_size, format_bytes (243 lines)
```

## Line Counts

| File | Before | After |
|------|--------|-------|
| main.rs | 1185 | 431 |
| create.rs | — | 162 |
| status.rs | — | 293 |
| snapshot.rs | — | 75 |
| disk.rs | — | 44 |
| lifecycle.rs | — | 66 |
| clone.rs | — | 41 |
| helpers.rs | — | 243 |
| instance_file.rs | 452 | 452 (untouched) |

## Verification

Docker: 80 passed, 0 failed, 7 ignored (daemon) + 30 passed (cli helpers) — identical to pre-decomposition baseline.
