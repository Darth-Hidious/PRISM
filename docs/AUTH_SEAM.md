# PRISM authentication seam

PRISM has one shared credential seam: `prism-runtime::auth`.

## Caller contract

A caller that needs a hosted platform calls `resolve_from_environment` with
`PrismPaths` and an optional configured API base. It receives
`ResolvedPlatformAuth` only when both a provider endpoint and credential can be
resolved:

- `PRISM_API_URL` selects the provider API endpoint. `PRISM_PLATFORM_URL` is
  the supported alternate spelling.
- `PRISM_API_KEY` is sent as `X-API-Key`; its contents are provider-defined and
  do not need an `m27_` prefix.
- `PRISM_TOKEN` / `PRISM_API_TOKEN` and stored session credentials preserve
  their resolved API-key-or-Bearer wire kind.
- All PRISM-native names take precedence over all historical aliases.
- `MARC27_API_URL`, `MARC27_PLATFORM_URL`, `MARC27_API_KEY`, `MARC27_TOKEN`,
  and `MARC27_API_TOKEN` remain supported deprecated aliases. A notice names
  the PRISM replacement when an alias supplies the selected value.
- `cli-state.json` is authoritative; the legacy SDK mirror is a compatibility
  fallback.
- No configured endpoint returns a clear `No platform configured` refusal;
  PRISM never silently chooses another company's endpoint.
- No credential returns `AUTH_REQUIRED` with the actions
  `export PRISM_API_KEY=<key>` or `prism login --token <PAT>`.

One compatibility exception preserves existing MARC27 installations without
restoring a global default: when a selected legacy MARC27 credential (or an
existing durable `m27_` node key) is the only provider evidence, PRISM selects
the known MARC27 adapter endpoint and emits the deprecation notice. A neutral
`PRISM_API_KEY` by itself does not select a provider; configure `PRISM_API_URL`
or `PRISM_PLATFORM_PROVIDER` as well.

Resolution is local-only. It never starts a device flow, opens a browser,
reads stdin, or polls for approval.

## Interactive auth boundary

The retained device-flow implementation remains in the CLI for existing users,
but it is reachable only through `prism login --interactive-auth` (or
`PRISM_ALLOW_INTERACTIVE_AUTH=1`) from a real TTY. It prints the verification
URL for manual opening and never launches a browser. Agent protocol, TUI, IPC,
headless, and non-TTY callers cannot enable it.

MARC27 is one optional provider adapter. It may continue producing the stored
`StoredCredentials` shape or `m27_` keys for compatibility, but PRISM owns the
credential precedence, wire-header semantics, endpoint refusal, and local
authorization model.
