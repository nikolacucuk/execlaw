# execlaw — macOS desktop bundle

A Tauri 2 menu bar app that wraps the execlaw control plane in a
`.app` bundle and registers it as a per-user LaunchAgent via
Apple's `SMAppService` API. Built only on macOS; the crate is
outside the main Rust workspace to keep `cargo check` cheap on
Windows/Linux dev hosts.

## What it ships

- `execlaw.app` containing:
  - `Contents/MacOS/execlaw-tray` — the menu bar UI (this crate).
  - `Contents/MacOS/execlaw` — the unmodified server binary from
    the root workspace, bundled as a Tauri "external binary."
  - `Contents/Library/LaunchAgents/com.execlaw.agent.plist` —
    injected by `scripts/build-mac.sh` (Tauri's bundler doesn't
    write into `Contents/Library/`).
  - `Contents/Resources/...` — generated icons + standard Tauri
    bundle metadata.
- `execlaw_<version>_aarch64.dmg` — drag-to-Applications installer.

## What it does

On launch:

1. Sets the macOS activation policy to **Accessory** — no Dock
   icon, no menu bar focus stealing.
2. Calls `SMAppService.agentServiceWithPlistName:"com.execlaw.agent.plist"`
   → `register()`. Idempotent; safe on every launch.
3. Builds the tray menu and starts a 5-second poller against
   `http://127.0.0.1:3031/api/ping`. The status row reflects
   whichever of {`Running`, `First-run setup pending`, `Setup
   wizard pending`, `Stopped`, `Error`, `needs approval`} applies.
4. On *Open execlaw* → opens a WKWebView window pointed at the
   local server. The SPA is served by the Rust binary via
   `rust-embed`, so the webview is same-origin with the API.

On *Uninstall execlaw…*: confirm → `SMAppService.unregister()` →
optional `~/.execlaw/` data wipe → quit.

On drag-to-Trash: **macOS auto-disables the agent** because every
service was registered through `SMAppService`. No leftover plist
in `~/Library/LaunchAgents/`.

## Building

The build is macOS-only and runs from the repo root:

```bash
./scripts/build-mac.sh
```

That script:

1. `npm --prefix web ci && npm --prefix web run build` — produces
   `web/dist/` which the server binary embeds.
2. `cargo build --release -p execlaw --target aarch64-apple-darwin`
   — produces the server binary.
3. Copies the server binary to
   `desktop-macos/src-tauri/bin/execlaw-aarch64-apple-darwin`.
4. `cd desktop-macos/src-tauri && cargo tauri build` — produces
   the `.app` and `.dmg`.
5. Post-bundle: copies the LaunchAgent plist into the bundle at
   `Contents/Library/LaunchAgents/`.

Outputs land under
`desktop-macos/src-tauri/target/release/bundle/`.

## Toolchain requirements

- macOS 13 or newer (SMAppService floor; you're building for it
  too).
- Xcode Command Line Tools (`xcode-select --install`).
- Rust 1.85+ (workspace `rust-version`).
- Node 20+ (Vite + SPA).
- Tauri CLI: `cargo install tauri-cli --version "^2.0"`.

## What's NOT in v1

- Code signing / notarization — the `.app` is ad-hoc signed.
  Operators bypass Gatekeeper with right-click → Open the first
  time.
- Auto-updater — operators download fresh `.dmg`s manually from
  GitHub Releases.
- Intel mac (`x86_64-apple-darwin`) — Apple Silicon only per
  [docs/setup-mac.md](../docs/setup-mac.md).
- Bundled Ollama / Docker / model weights — operators install
  separately.
- Voice mic Info.plist key (`NSMicrophoneUsageDescription`) —
  added when Phase 8 voice UI ships.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H028: packaged release qualification](../docs/llm-harness-roadmap.md#enhancement-028).
- [H073: macOS isolation](../docs/llm-harness-roadmap.md#enhancement-073).
- [H075: safe service draining](../docs/llm-harness-roadmap.md#enhancement-075).
- [H114: reproducible builds](../docs/llm-harness-roadmap.md#enhancement-114).
- [H116: air-gapped updates](../docs/llm-harness-roadmap.md#enhancement-116).
- [H117: service authority](../docs/llm-harness-roadmap.md#enhancement-117).
- [H118: machine-loss recovery](../docs/llm-harness-roadmap.md#enhancement-118).

Production encryption verification remains pending under
[F04](../docs/llm-harness-roadmap.md#finding-f04).
The reviewed packaging scripts omit the SQLCipher feature; do not infer
encrypted-at-rest protection from a successful desktop build. H028 requires
verification of the actual packaged binary before release qualification.
