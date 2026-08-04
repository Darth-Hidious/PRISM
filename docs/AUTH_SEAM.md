# PRISM authentication seam

PRISM has one shared credential seam: `prism-runtime::auth`.

## Caller contract

A caller that needs the hosted platform calls `resolve_from_environment` with
`PrismPaths` and the default API base. It receives `ResolvedPlatformAuth`:

- `MARC27_API_KEY` (the frozen `m27_` prefix) is sent as `X-API-Key`.
- `MARC27_TOKEN` / `MARC27_API_TOKEN` and stored session credentials are sent
  as `Authorization: Bearer ...`.
- `cli-state.json` is authoritative; the legacy SDK mirror is a compatibility
  fallback.
- No credential returns `AUTH_REQUIRED` with the exact actions
  `export MARC27_API_KEY=m27_your_key_here` or
  `prism login --token <PAT>`.

Resolution is local-only. It never starts a device flow, opens a browser,
reads stdin, or polls for approval.

## Interactive auth boundary

The retained device-flow implementation remains in the CLI for existing users,
but it is reachable only through `prism login --interactive-auth` (or
`PRISM_ALLOW_INTERACTIVE_AUTH=1`) from a real TTY. It prints the verification
URL for manual opening and never launches a browser. Agent protocol, TUI, IPC,
headless, and non-TTY callers cannot enable it.

A future standalone `marc27` CLI can replace the retained device-flow
implementation without changing any platform command, TUI, agent, or IPC
caller: it must produce the same stored `StoredCredentials` shape or provide a
headless `m27_` API key, while this seam and its wire-header rules remain
unchanged.
