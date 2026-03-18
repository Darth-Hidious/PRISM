## 2026-03-19 - Prism Node SSH Capability

### Summary
Added an SSH advertisement capability to `prism node up` and `prism-node up` so a node can publish a reachable SSH endpoint as part of its node services when connecting to the MARC27 platform.

This does not open ports, provision SSH, or manage access keys. It advertises an already-available SSH endpoint in the node's registered capability set.
As of the latest update, SSH advertisement is also bound to the logged-in PRISM `user_id` and marked as an owner-consent capability in node labels.

### Files Changed
- `crates/cli/src/main.rs`
  - Added `--ssh-host`, `--ssh-port`, and `--ssh-user` flags to `prism node up`.
  - Forwarded SSH flags through the `--background` re-exec path.
  - Passed SSH settings into `prism_node::daemon::DaemonOptions`.
  - Added a small unit test for default SSH user resolution.
- `crates/node/src/main.rs`
  - Added `--ssh-host`, `--ssh-port`, and `--ssh-user` to `prism-node up`.
  - Passed SSH configuration into the daemon options.
- `crates/node/src/daemon.rs`
  - Added `SshCapability`.
  - Injected an `ssh` service advertisement into node capabilities during daemon startup.
  - Formats endpoints as `ssh://host:port` or `ssh://user@host:port`.
  - Requires a persisted logged-in `user_id` before SSH can be advertised.
  - Tags the capability with:
    - `ssh.enabled=true`
    - `ssh.consent_mode=owner_user_id_required`
    - `ssh.owner_user_id=<logged-in-user-id>`
  - Preserves `ssh://...` endpoints when building the wire-safe capability payload.
  - Added a unit test to verify SSH service advertisement.

### Usage
Native PRISM CLI:
```bash
prism node up --ssh-host node.example.com --ssh-user sid --ssh-port 22
```

Direct node binary:
```bash
prism-node up --ssh-host node.example.com --ssh-user sid --ssh-port 22
```

### Notes
- `--ssh-host` enables the advertisement.
- `--ssh-port` defaults to `22`.
- If `--ssh-user` is not provided, PRISM falls back to the current `$USER` when available.
- SSH advertisement is attached when the node registers with the platform. It is not a local system probe result.
- SSH advertisement now fails closed if there is no logged-in PRISM user with a persisted `user_id`.

### Validation
- `cargo test -p prism-node -p prism-cli -- --nocapture`
- `./target/debug/prism node up --help`
