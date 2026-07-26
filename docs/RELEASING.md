# Releasing PRISM

## Cutting a release

```bash
git tag vX.Y.Z && git push origin vX.Y.Z
```

That is the whole thing. `.github/workflows/native-release.yml` triggers on
`v*` tags and publishes five binary archives plus the Python wheel to the
GitHub release.

`install.sh` and `install.ps1` resolve "latest" through
`GET /repos/Darth-Hidious/PRISM/releases/latest`, which GitHub defines as the
most recent non-draft, non-prerelease release **by publication date, not by
version number**. That matters here: this repo has both `v1.0.0` (Jul 2026)
and `v2.7.1` (May 2026), and the API correctly reports `v1.0.0` as latest.
Any new tag published today becomes latest regardless of how it sorts
against `v2.7.x`.

### Before you tag

- [ ] `dashboard/` builds — the release job runs `npm ci && npm run build`
      and a failure there kills every platform.
- [ ] The version in `Cargo.toml` matches the tag. `ensure_venv` builds the
      Python wheel URL from `CARGO_PKG_VERSION`
      (`crates/python-bridge/src/venv.rs`), so a mismatch means every fresh
      install fails to find its wheel and silently falls back to a git
      install.
- [ ] All five `Package Native` jobs are green. They are `fail-fast: false`,
      so a partial failure still publishes a partial release — which is
      exactly how v1.0.0 shipped with no Intel Mac build.

## What ships

| Archive | Built on | Runs on |
|---|---|---|
| `prism-linux-x86_64.tar.gz` | `ubuntu-22.04` | glibc 2.35+ |
| `prism-linux-aarch64.tar.gz` | `ubuntu-22.04-arm` | glibc 2.35+ |
| `prism-macos-aarch64.tar.gz` | `macos-15` | macOS 11+, Apple Silicon |
| `prism-macos-x86_64.tar.gz` | `macos-15-intel` | macOS 11+, Intel |
| `prism-windows-x86_64.zip` | `windows-latest` | Windows 10+ x64 (and ARM64 under emulation) |
| `prism_platform-*.whl` | `ubuntu-latest` | Python 3.11+ |

Two separate macOS archives, not one `universal2` binary. A universal binary
needs a working x86_64 slice either way, so `lipo` solves nothing on its own;
it would double the download for the Apple Silicon majority; and the two
slices genuinely differ — only the arm64 one has the native embedding model
(see below). A fat binary would hide that difference. When GitHub retires
x86_64 macOS runners (Aug 2027) the Intel row simply gets dropped.

## Platform support policy

**Linux binaries must be built on the oldest glibc we support.** A binary
linked against glibc runs on that version or newer, never older. Building on
`ubuntu-latest` (24.04, glibc 2.39) produced artifacts that fail outright on
Ubuntu 22.04 LTS and Debian 12:

```
/lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found
/lib/x86_64-linux-gnu/libstdc++.so.6: version `CXXABI_1.3.15' not found
```

Ubuntu 22.04 LTS is the most common institutional Linux, so this was a total
silent loss of that audience. `ubuntu-22.04` (glibc 2.35, GCC 11) covers
Ubuntu 22.04+, Debian 12+ and SLES 15 SP6+.

**Not covered:** RHEL 9 and Rocky 9 sit at glibc 2.34, RHEL 8 at 2.28. Those
need either a build in an older container (e.g. `quay.io/pypa/manylinux_2_28`)
or `cargo-zigbuild --target x86_64-unknown-linux-gnu.2.28`. Neither is wired
up. Users there build from source.

**Runner deprecations to watch:**

| Label | Support ends |
|---|---|
| `macos-14` | 2026-11-02 — already migrated off |
| `ubuntu-22.04` | 2027-04-17 |
| `macos-15-intel` | Aug 2027, last x86_64 macOS image ever |

### Intel macOS has no local embedding model

`fastembed` pins `ort = "=2.0.0-rc.12"`, and pyke stopped publishing
`x86_64-apple-darwin` prebuilts at rc.11 (upstream ONNX Runtime dropped its
macOS x86_64 tarball at v1.25.0). The build script hard-fails, which is why
the `macos-x86_64` job failed for v1.0.0 and no Intel Mac build has ever
shipped.

`crates/embed/Cargo.toml` now excludes `fastembed` on that single target.
Consequences on Intel Macs only: semantic search falls back to keyword-only
unless `PRISM_EMBED_BACKEND=openai` is configured. Everything else is
identical, and the binary is ~35 MB smaller.

The alternative — `fastembed`'s `ort-load-dynamic` feature plus a
self-compiled `libonnxruntime.dylib` shipped alongside the binary — is the
only way to restore it, and it means owning an ONNX Runtime build.

## Code signing

### macOS — what it is today

Binaries are **ad-hoc signed** (`codesign -s -`), which is not a Developer ID
signature and is not notarization:

```
$ codesign -dv prism
Signature=adhoc
TeamIdentifier=not set
```

`install.sh` works anyway because it strips `com.apple.quarantine` after
extracting. A user who downloads the tarball from the Releases page by hand
gets *"Apple could not verify this app is free of malware"* until they run
`xattr -d com.apple.quarantine prism` — which the README now documents.

### macOS — what real notarization requires

Owner decisions and costs, so it is not done here:

1. **Apple Developer Program — $99/year.** Enrolling as an organization
   requires a D-U-N-S number (free, ~1-2 weeks to obtain). Individuals and
   sole proprietors do not need one. Nonprofit/education/government fee
   waivers exist.
2. **A "Developer ID Application" certificate**, exported as `.p12`.
3. **GitHub secrets**: the base64 `.p12`, its password, a keychain password,
   and either an Apple ID + app-specific password + team ID, or an App Store
   Connect API key (issuer ID, key ID, `.p8`).
4. **Workflow changes** in the `Assemble package` step, macOS legs only —
   replace `codesign -s -` with:

   ```bash
   codesign --options runtime --timestamp \
            -s "Developer ID Application: <NAME> (<TEAMID>)" prism prism-node
   ditto -c -k --keepParent package prism-macos-<arch>.zip
   xcrun notarytool submit prism-macos-<arch>.zip \
         --apple-id "$APPLE_ID" --password "$APP_PASSWORD" \
         --team-id "$TEAM_ID" --wait
   ```

   Hardened runtime (`--options runtime`) and a secure timestamp
   (`--timestamp`) are both mandatory; omitting either fails notarization.

5. **Stapling needs a different container.** Apple's docs are explicit that
   tickets cannot be stapled to standalone binaries, and cannot be stapled to
   a `.zip` either. To ship a stapled artifact the CLI has to go inside a
   `.pkg` or `.dmg`. Without a staple, Gatekeeper still clears a notarized
   binary — it just has to reach Apple's servers to fetch the ticket, so the
   very first run on a machine that is offline will fail.

Rough total: **$99/year plus a day of CI work**, and a change of archive
format if offline first-run matters.

### Windows

The `.exe` is unsigned. This matters less than it looks:

- Launched from PowerShell or Command Prompt — how `install.ps1` installs it
  and how users run `prism` — SmartScreen's reputation check does not fire.
  It hooks `ShellExecuteEx` (Explorer's double-click path), not
  `CreateProcess`.
- `Invoke-WebRequest` does not write a mark-of-the-web, and neither
  `Expand-Archive` nor .NET's `ZipFile` propagate one, so the installed
  binaries carry no MOTW at all.
- Double-clicking `prism.exe` in Explorer **does** show *"Windows protected
  your PC"*. Documented in the README.

If signing is wanted anyway: OV certificates run roughly $44-530/year
depending on vendor and term, EV roughly $250-745/year. **EV no longer buys
instant SmartScreen reputation** — Microsoft removed that behaviour in ~2024
and now treats OV and EV identically, so the EV premium is not justified for
this purpose. Since June 2023 all code-signing keys must live on FIPS 140-2
Level 2 hardware, so a `.pfx` in a GitHub secret is no longer possible;
signing from CI means a cloud HSM service (Azure Trusted Signing at ~$10/mo
is the cheapest, or DigiCert KeyLocker / SSL.com eSigner, both of which have
GitHub Actions).

## The website install URLs

`prism.marc27.com` is a Vercel site served from the **PRISM-pitch** repo, not
this one. `vercel.json` there rewrites `/install.sh` and `/install.ps1` to
`raw.githubusercontent.com/Darth-Hidious/PRISM/main/…`.

Two consequences:

- Installer changes go live when they land on `main`, independent of any
  release tag.
- A new installer file needs a new rewrite entry in PRISM-pitch **and** a
  deploy of that site, or the URL 404s.
