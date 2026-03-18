## 2026-03-19 - Prism Node Signed SSH Claims

### Summary
Added a second node identity key for service-signing and used it to attach a signed SSH ownership claim to `prism-node` registrations.

This moves PRISM Node one step closer to a Tailscale-like model:
- node identity is not just a transport endpoint
- SSH advertisement is bound to the logged-in owner user id
- the advertised SSH capability carries a signed, machine-readable claim bundle

### Files Changed
- `Cargo.toml`
  - Added `ed25519-dalek` as a workspace dependency.
- `crates/node/Cargo.toml`
  - Added `ed25519-dalek` to node dependencies.
- `crates/node/src/crypto.rs`
  - Added Ed25519 signing key support alongside the existing X25519 key agreement key.
  - Added load/generate, rotate, encode/decode, sign, and verify helpers.
  - Added unit tests for signing key generation and signature verification.
- `crates/node/src/daemon.rs`
  - Loads a signing keypair at startup.
  - Publishes `identity.signing_public_key` in node labels.
  - Builds a signed SSH claim payload containing owner id, org id, node name, endpoint, and issuance timestamp.
  - Publishes `ssh.claim_payload`, `ssh.claim_signature`, `ssh.claim_algorithm`, and `ssh.claim_version` labels.
  - Preserves `ssh://...` endpoints in the wire-safe capability payload.
- `crates/cli/src/main.rs`
  - Existing `prism node up --ssh-*` surface remains the public entrypoint for SSH advertisement.

### Live Validation
A real foreground node registration was tested against `https://api.marc27.com`:
- command used:
  - `./target/debug/prism node up --name codex-ssh-smoke-foreground --no-compute --no-storage --ssh-host 127.0.0.1 --ssh-user siddhartha --ssh-port 22`
- platform accepted the node and returned a real node id.
- `GET /api/v1/nodes/live` showed:
  - SSH service endpoint `ssh://siddhartha@127.0.0.1:22`
  - `identity.signing_public_key`
  - `ssh.claim_payload`
  - `ssh.claim_signature`
  - `ssh.claim_algorithm=ed25519`
  - `ssh.owner_user_id=<logged in user id>`

The smoke-test node was then shut down and confirmed absent from `/api/v1/nodes/live`.

### Notes
- This does not yet create an encrypted mesh or SSH broker.
- It does provide a verifiable service-identity substrate the platform can build on.
- Docker-based job execution was not live-tested in this pass because the Docker daemon was not running on the machine.

### Validation
- `cargo fmt --all`
- `cargo test -p prism-node -p prism-cli -- --nocapture`
- live registration against `api.marc27.com`
- `curl https://api.marc27.com/api/v1/nodes/live`
