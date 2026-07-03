---
name: use
description: Switch where chat turns are routed. `marc27 [--model X]` (default), `local --url <openai-compat> --model <name>`, `provider <openai|mistral|gemini|cohere> --model <name>`, `show`, or `reset`. Identical to `prism use ...` from the shell — both write `~/.prism/config.toml`.
---

The user just typed `/use $ARGUMENTS` from inside the PRISM chat.

Run `prism use $ARGUMENTS` via the bash tool. Capture both stdout and
the exit code. Then output, on a single line each, in PRISM voice:

  1. The literal stdout of `prism use $ARGUMENTS` (no extra prefix).
  2. If exit was non-zero, the stderr too.
  3. A reminder that the change applies to the **next** chat turn —
     in-flight streams keep using the previous chat target. To pick
     up the new target immediately, type `/new` to start a fresh
     conversation, or `/exit` and relaunch `prism`.

Don't editorialise. Don't add suggestions about what model to pick
unless the user explicitly asked. The `prism use` CLI already
prints the right message; pass it through.

Note: this is the markdown-template MVP. A future native
`AppCommand::Use` will hot-swap the running platform_bridge's
`Arc<RwLock<ChatTarget>>` so the swap takes effect on the same turn
without `/new`. For now, the persisted config + `/new` is the
shipped UX.
