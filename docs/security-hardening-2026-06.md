# Security Hardening — 2026-06

Comprehensive security review and hardening pass applied in 2026-06.
All findings are graded by severity. Implemented changes are marked **DONE**;
deferred items note the design gap and a recommended next step.

---

## Summary of changes

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| 1 | HTTP security headers missing | Medium | **DONE** |
| 2 | No brute-force protection on `/api/login` | Medium | **DONE** |
| 3 | Homoglyph fold covers Cyrillic only | Low | **DONE** |
| 4 | Webhook `auth=None` logged at WARN, not ERROR | Low | **DONE** |
| 5 | `accesses_sensitive_data` hardcoded `false` in chat dispatch | Medium | Deferred — architecture gap |
| 6 | JWT tokens in `localStorage` | Medium | Accepted for localhost; CSP mitigates |
| 7 | `~/.execlaw/master.key` permissions not enforced | Low | Deferred |

---

## Finding 1 — HTTP security headers (DONE)

**What was wrong:** No response included `X-Frame-Options`, `X-Content-Type-Options`,
`Referrer-Policy`, or `Content-Security-Policy`. A compromised renderer or a
cross-origin iframe could exfiltrate tokens or perform click-jacking.

**Fix:** Added `security_headers` middleware in `crates/server/src/routes.rs`
applied as a `.layer(axum::middleware::from_fn(security_headers))` wrapping the
entire Axum router. Headers injected via `entry().or_insert()` so per-route
overrides (e.g. `no-referrer` on attachment downloads) are not clobbered.

Headers added to every response:

| Header | Value |
|--------|-------|
| `X-Frame-Options` | `DENY` |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `strict-origin` (default; routes may set `no-referrer`) |
| `Content-Security-Policy` | `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; object-src 'none'` |

**Files changed:**
- `crates/server/src/routes.rs` — added `security_headers()` function and layer.

---

## Finding 2 — Login brute-force protection (DONE)

**What was wrong:** `POST /api/login` had no rate limiting. An attacker on
the local network (or via a compromised process with loopback access) could
submit unlimited Argon2id attempts in parallel to brute-force the admin
password.

**Fix:** New module `crates/server/src/auth_rate_limit.rs` implementing
`LoginRateLimiter` — a DashMap-backed per-IP sliding-window token bucket.

- 5 failed attempts allowed per 10-minute window.
- The check runs BEFORE Argon2id so rejected attempts do not trigger the
  intentionally-slow hash (removes timing oracle).
- A successful login calls `reset(ip)` to clear the bucket (legitimate user
  who mistyped is not permanently locked out).
- Returns HTTP 429 `login_rate_limited` with `Retry-After` seconds in the
  error body.
- `PeerIp` custom extractor reads from `ConnectInfo<SocketAddr>` in production
  (requires `into_make_service_with_connect_info`, now set in `cli/main.rs`)
  and falls back to a stable test sentinel in unit tests.

**Files changed:**
- `crates/server/src/auth_rate_limit.rs` — new module (`LoginRateLimiter`, `PeerIp`).
- `crates/server/src/lib.rs` — `pub mod auth_rate_limit;`.
- `crates/server/src/state.rs` — `pub login_limiter: LoginRateLimiter` in `AppState`.
- `crates/server/src/routes.rs` — `login()` handler checks/resets limiter.
- `crates/cli/src/main.rs` — `login_limiter: LoginRateLimiter::new()` in `AppState`
  construction; `into_make_service_with_connect_info` on `axum::serve`.
- All `crates/server/tests/*.rs` that construct `AppState` directly.

**Tests:**
- `auth_rate_limit::tests` — 5 unit tests covering: within-limit, exceeded,
  reset, IP isolation, purge-expired.
- `auth_rate_limit::extractor_tests` — verifies `PeerIp` sentinel fallback.

---

## Finding 3 — Homoglyph fold coverage (DONE)

**What was wrong:** `crates/policy/src/input_guard.rs::fold_common_homoglyphs()`
only folded 14 Cyrillic characters to ASCII. Greek and Armenian look-alikes
(Α/Β/Ε/Ζ/Η/Ι/Κ/Μ/Ν/Ο/Ρ/Τ/Υ/Χ and lowercase α/ε/ο/ρ/τ/υ/ν) were not
covered, leaving a gap for adversarial prompts using Greek lookalikes.

**Fix:** Expanded the match arm to cover:
- Greek lowercase: α→a, ε→e, ο→o, ρ→p, τ→t, υ→u, ν→v
- Greek uppercase: Α→A, Β→B, Ε→E, Ζ→Z, Η→H, Ι→I, Κ→K, Μ→M, Ν→N, Ο→O, Ρ→P, Τ→T, Υ→Y, Χ→X
- Armenian: հ→h, ո→n

**Files changed:**
- `crates/policy/src/input_guard.rs` — expanded match arm + 3 new tests.

**Tests added:** `folds_greek_lowercase_lookalikes`, `folds_greek_uppercase_lookalikes`,
`folds_greek_rho_to_p`.

---

## Finding 4 — Webhook auth=None log elevation (DONE)

**What was wrong:** When a plugin's `[[webhook_routes]]` entry has no `auth`
field (omitted, not explicitly `auth = "none"`), `verify_webhook_auth()` emitted
a per-request `warn!` and passed the request through. The WARN level is easily
missed in noisy logs.

**Fix:** Elevated to `error!` level and expanded the message to clarify the
operator action required (`add auth = "none"` to opt-in explicitly, or configure
real auth). This makes unintended open webhook routes immediately visible in
log viewers and SIEM tooling.

**Files changed:**
- `crates/server/src/plugin_webhook_routes.rs` — `warn!` → `error!` with
  expanded message.

---

## Finding 5 — `accesses_sensitive_data` hardcoded false (Deferred)

**What:** In `crates/server/src/chats.rs` around line 222, the `RuleOfTwoInput`
built for the web-chat path hardcodes `accesses_sensitive_data: false` and
`produces_external_effect: false`. The Rule of Two gate therefore never fires
for web-chat turns, even when a sensitive tool (vault read, contact lookup) is
called.

**Why deferred:** The `RuleOfTwoInput` is evaluated PRE-DISPATCH — before the
model has chosen any tools. The correct fix requires either:
- Post-dispatch re-evaluation (requires a design change to the turn loop in
  `crates/runner-local/src/turn.rs` and `crates/server/src/chats.rs`), OR
- Marking tools with `sensitive = true` in the tool descriptor schema
  (`crates/plugin-sdk/src/manifest.rs`) and propagating the flag to a
  pre-turn capability scan.

Both are non-trivial and carry test-surface risk. The finding is documented;
the next sprint should implement the `sensitive` flag on tool descriptors and
thread it through the capability registry before computing `RuleOfTwoInput`.

**Risk:** Low in the current loopback-only deployment (single operator,
localhost). Elevated if the server is ever exposed via a reverse proxy to the
internet.

---

## Finding 6 — JWT in localStorage (Accepted)

**What:** `web/src/auth/tokens.ts` stores the JWT access token and refresh
token in `localStorage` under `execlaw.access_token` and
`execlaw.refresh_token`. This is vulnerable to XSS — any malicious script
running on the page can steal them.

**Mitigation in place:** The `Content-Security-Policy` header added in
Finding 1 disables inline scripts and restricts `script-src` to `'self'`,
significantly reducing the XSS surface. The server is designed to bind
`127.0.0.1` only (not exposed on the public interface).

**Next step (Phase 7):** Migrate to httpOnly sameSite=strict cookies. This is
already noted in `web/src/auth/tokens.ts` as the Phase 7 plan and tracked in
the main roadmap. No code change in this pass.

---

## Finding 7 — master.key file permissions (Deferred)

**What:** `crates/vault/src/keyring_key.rs` calls `std::fs::create_dir_all`
to ensure `~/.execlaw/` exists before writing the SQLCipher master key, but
does not explicitly set directory permissions to `0700`. On a shared machine
or a container with permissive umask, other local users could read the key.

**Why deferred:** Permission setting is OS-specific (Unix `chmod` vs Windows
ACL). The operator's machine is a single-user Windows workstation where the
filesystem default (`CREATOR OWNER` full control) already restricts access.

**Next step:** Add `#[cfg(unix)]` chmod(0700) call in `keyring_key.rs` after
`create_dir_all`. On Windows, set an explicit DACL via `winapi` or `windows-permissions`
crate. Track as a follow-up hardening item.

---

## Operator notes

1. **Webhook plugins**: any plugin that omits `auth` from its
   `[[webhook_routes]]` table will now emit `ERROR`-level logs on every
   inbound request. Open `plugins/<id>/plugin.toml`, add `auth = "none"` to
   each `[[webhook_routes]]` entry that is intentionally unauthenticated (e.g.
   WhatsApp /health pings), or configure a real auth method.

2. **CSP and existing plugins**: The new CSP header may block plugin UI
   panels that load assets from external CDNs. If a plugin panel stops
   rendering, check the browser console for CSP violations and update the
   plugin's assets to be served from `self`, or request a CSP relaxation via
   an operator config key (planned for Phase 7).

3. **WebAuthn**: The server already has WebAuthn code wired
   (`crates/server/src/webauthn.rs`). Registering a passkey for your admin
   account is the single highest-impact security improvement you can make for
   a remotely-accessible deployment. Do this via `Settings → Security →
   Add passkey` in the SPA.
