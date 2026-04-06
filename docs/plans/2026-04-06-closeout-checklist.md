# PRISM Close-Out Checklist

**Date:** 2026-04-06
**Status:** Active

## Completed

### Dashboard
- Fixed dashboard session bootstrap from URL-backed session tokens.
- Added authenticated request headers in the dashboard API client.
- Fixed WebSocket protocol handling and query invalidation keys.
- Reworked the main node page into a PM-facing operations summary.
- Added explicit session-gated preview states instead of failing protected routes noisily.
- Verified with `npm run build` in `dashboard/`.

### Packaging and Updates
- Removed the fake `dashboard/dist` placeholder from native release packaging.
- Native release now builds the real dashboard bundle before Rust packaging.
- Native release now publishes standalone `prism-tui-*` binaries alongside tarballs.
- `app/update.py` now prefers standalone TUI assets and falls back to extracting `prism-tui` from release archives.
- `install.sh` now marks `prism-tui` executable after extraction.
- Verified with `.venv/bin/python -m pytest tests/test_update.py -q`.

### Verification
- Verified `cargo test -p prism-agent --quiet`
- Verified `cargo test -p prism-cli --quiet`
- Verified `cargo test -p prism-node --quiet`
- Verified `cargo build -p prism-cli`
- Verified `bun run build` in `frontend/`
- Verified `npm run build` in `dashboard/`

## Remaining Real Work

### Jupyter / Kernel Integration
**Status:** Not implemented in this repository yet.

Current repo state:
- Jupyter is only detected as installed software in `crates/node/src/detect.rs`.
- There is no actual kernel client, notebook runtime, or notebook execution subsystem in the current PRISM codebase.
- The existing references are design-level only, primarily in `docs/plans/2026-04-04-frontend-design.md`.

This means the remaining Jupyter item is not a close-out bugfix. It is a real implementation track for the IDE layer.

### Recommended Next Tasks
1. Add a VS Code / VSCodium extension-side notebook integration plan.
2. Decide whether PRISM will embed the VS Code Jupyter extension model or implement a narrower ZMQ kernel bridge.
3. Define agent access rules for notebook read/write/execute once that kernel layer exists.

## External Dependencies Still Outside This Repo
- Marketplace info/install remains dependent on the platform catalog being populated.
- `discourse turns` still depends on the platform returning populated turn rows.
- Ingest can be improved further once platform-side per-page PDF slices and per-corpus stats are live.
