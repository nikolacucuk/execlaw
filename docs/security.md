# execlaw — Security

## Implementation commitment and current limits

The [implementation plan](implementation-plan.md) tracks the committed
H001-H154 program; the [roadmap findings](llm-harness-roadmap.md#review-findings-and-unresolved-verification)
are the authoritative record of open issues F01-F19. Planned controls are not
current guarantees. In particular, F01/F19 identify missing authorization on
plugin and automation routes, F02 unsafe plugin upgrades, F03 fetch SSRF,
F04 unqualified SQLCipher packaging, and F05 incomplete session revocation.
Loopback access and OpenAPI security declarations do not authenticate callers.
F18 also means the current automation test-run is not an effect-free preview.

Security implementation is coordinated through
[H025 network egress](llm-harness-roadmap.md#enhancement-025),
[H026 authorization](llm-harness-roadmap.md#enhancement-026),
[H027 plugin isolation/upgrades](llm-harness-roadmap.md#enhancement-027), and
[H028 release qualification](llm-harness-roadmap.md#enhancement-028).
The subsequent work covers [H051 provenance](llm-harness-roadmap.md#enhancement-051),
[H052 dispatch-time authority](llm-harness-roadmap.md#enhancement-052),
[H054 secret brokering](llm-harness-roadmap.md#enhancement-054),
[H055 outbound data checks](llm-harness-roadmap.md#enhancement-055),
[H072 runtime isolation](llm-harness-roadmap.md#enhancement-072),
[H076 key/secret lifetime](llm-harness-roadmap.md#enhancement-076),
[H082 publisher revocation](llm-harness-roadmap.md#enhancement-082), and
[H126 effect-free automation scenarios](llm-harness-roadmap.md#enhancement-126).
Completion requires the corresponding implementation and adversarial evidence
in the tracker, including full-router authorization tests and zero-effect
simulation tests. This documentation update does not close those findings.

This document describes:

1. The disclosure path for security issues.
2. The threat model — what we defend against, what we don't.
3. The cryptography in use.
4. Trust assumptions about plugins, operators, and contacts.
5. Known limitations and unresolved attack vectors.

It complements [`docs/architecture.md`](architecture.md), which covers
the structural design choices that flow from these security postures.

---

## 1. Reporting a vulnerability

> **Do not open a public GitHub issue for a security report.**

Email the maintainer at the address in the repository's `package`
metadata or top-level `Cargo.toml` `authors` field, with the subject
line prefixed `[execlaw security]`. If you don't get an
acknowledgement within 72 hours, escalate via a GitHub Security
Advisory — go to the repo's Security tab → Advisories → Report a
vulnerability.

What to include:

- Affected version (commit SHA from `git rev-parse HEAD` or the
  release tag).
- Reproduction steps with the smallest example that exhibits the
  issue.
- Your assessment of impact (information disclosure, RCE, privilege
  escalation, denial of service, …).
- Whether you're willing to be credited in the fix announcement, and
  if so, how to credit you.

What to expect:

- Initial acknowledgement within 72 hours.
- A maintainer-side severity assessment within one week.
- For confirmed Critical / High issues: a coordinated disclosure
  window of up to 90 days from acknowledgement, during which we'll
  patch on `foundation`, prepare an advisory, and coordinate
  release. For Medium and Low: we'll patch on `foundation` and ship
  in the next regular release.
- Public advisory + CVE assignment when the patch ships.

Please do not exploit the vulnerability against any execlaw install
you don't own, do not exfiltrate data, and do not pivot from the
vulnerability to other targets. Good-faith research within those
limits is welcome.

There is no bug bounty.

---

## 2. Threat model

execlaw is a **single-operator self-hosted agent platform**. The
threat model is shaped by that fact: there is one administrator, and
the host machine is assumed to be physically and digitally controlled
by that administrator. Multi-tenant abuse, server-side privilege
escalation between tenants, and "evil platform operator" attacks are
out of scope because there is no platform operator distinct from the
user.

### What we defend against

- **Inbound prompt injection** from contacts on bridged transports
  (Signal / WhatsApp / SMS / Slack / email). Untrusted text never
  reaches any tool-using model role unmodified — see
  [`docs/agent-model.md` §8](agent-model.md) (planner/executor split)
  and the spotlighting layer in `crates/policy/src/spotlighting.rs`.
- **First-contact attack vector** — a stranger messaging the agent
  for the first time cannot drive any model inference. The cold-
  contact escalation flow parks the conversation in
  `AwaitingTrustDecision` until the controller decides — see
  [`docs/architecture.md` §9.3](architecture.md).
- **Compromised conversation runner** — the per-conversation runner
  container holds an Ed25519-signed JWT scoped to exactly one
  `(conversation_id, turn_seq)`. A poisoned context cannot reach
  cross-conversation memory, cross-conversation state, or any tool
  outside the capability set the policy engine granted for that
  turn.
- **Tampered event log** — legacy integrity-v1 rows retain their independent
  HMAC-SHA256 interpretation. Integrity-v2 rows form a per-conversation chain
  over the complete canonical row, key id, and predecessor tag. A separately
  signed durable head records the terminal sequence and tag. Replay verifies
  the complete chain and head without invoking model or plugin code;
  mutation, deletion, insertion, reordering, or truncation before that head
  surfaces as `DbError::TamperDetected`. The running service loads its event
  signer from the separate `event-hmac.key`; protect it with the recovery keys.
- **Encrypted state at rest** — production builds must enable the
  `sqlcipher` Cargo feature to enforce encryption. The reviewed desktop build
  commands omit that opt-in feature (F04); do not infer encryption from the
  release label or a successful `PRAGMA key`. The master-key loader keeps a
  durable `master.key` file and mirrors it into the OS keyring. Packaged-binary
  encryption verification and fail-closed startup remain H028 work.
- **Forged approval responses** — every cold-contact / sensitive-
  tool approval emits a JWT-signed `approval_token`. The respond
  endpoint verifies the token's `jti` matches the approval id;
  guessing an approval id alone is not enough to forge a verdict.
- **Replay of outbound messages** — outbox rows carry a framework-
  minted idempotency key derived from `(conversation_id, turn_seq,
  tool_call_ordinal)`. The LLM cannot influence the key; transport
  plugins use it to dedup at delivery.
- **Cross-trust memory leakage** — `memory_entries` is keyed on
  `(scope, trust_class, key)`. A `KnownTrusted` caller cannot read
  `Controller`-scoped memory rows; the read-down cascade is
  enforced at the storage shim (`crates/core/src/tool_apis.rs`),
  not the tool layer.
- **Webhook spoofing** — `[[webhook_routes]]` are unauthenticated
  by design (third-party services don't carry execlaw JWTs), but
  every plugin handler validates a per-install secret via
  constant-time comparison against a vault-stored value. The
  WhatsApp plugin's `on_webhook_event` is the canonical pattern.
- **Cloud LLM exfiltration** — there is no cloud LLM code path.
  Inference is local-only against an OpenAI-compatible endpoint
  (vLLM / OpenArc / Whisper / Kokoro). Removing the rule is not a
  configuration option; it requires editing source.
- **Local-service SSRF and DNS rebinding** — the shared endpoint policy accepts
  loopback plus only Controller-approved CIDRs and DNS names from SQLite. Every
  DNS answer must be local/approved, accepted answers are pinned into the HTTP
  client, redirects are disabled, and userinfo plus alternate numeric hosts are
  rejected. Resolution classifications, addresses, and failures are persisted
  for operator inspection. Configured inference, HTTP MCP, Graphiti, and voice
  STT/TTS use this path.
- **Public web-fetch SSRF and DNS rebinding** — web fetch and research resolve
  every answer, reject non-global and special-purpose addresses, pin the direct
  HTTP connection, and validate each redirect before connecting. Ambient
  proxies are disabled on checked clients because proxy-side DNS would bypass
  the pin. Script-plugin HTTP uses the same per-connection resolver and permits
  private destinations only through the configured CIDR/DNS approvals; sidecar
  HTTP resolves only a currently published supervised-sidecar host and port.
- **Untraceable injected memory** — current memory projection accepts only an
  approved assertion with at least one append-only evidence row. Evidence binds
  the assertion to a conversation event, payload path, and quote hash; trust is
  filtered in SQL before ranking.
- **Artifact substitution** — bundled plugin ZIPs require an allowlisted
  identity/repository/workflow, matching ZIP and SBOM digests, and offline
  cosign verification of SLSA provenance. Subprocess bytes are checked at
  spawn, and sidecar/runner OCI references must be digest-pinned and have a
  verified provenance row. The only bypass is a persisted Controller-enabled
  local-development override, and every use is audited.
- **Plugin archive expansion** — uploaded and bundled plugin ZIPs are staged
  only after path containment checks, a 4,096-entry cap, a 64 MiB per-file
  expanded-size cap, and a 256 MiB total expanded-size cap. Plugin versions
  used in staging paths accept only bounded, path-safe components.

### What we explicitly do NOT defend against

- **Compromised host machine.** If an attacker has root / Admin on
  the machine running execlaw, they can read the SQLCipher key from
  process memory, dump the OS keyring, or replace the binary. Host
  compromise is total compromise. We rely on the operator's host
  hygiene.
- **Malicious plugins.** Plugins are *trusted code* (see §4 below).
  An installed plugin can read the vault, mint outbound messages,
  scrape memory, and inject events. The control surface for malice
  is "don't install plugins from sources you don't trust" — there
  is no in-process sandbox for the script tier (Rhai runs in the
  same process as the host) and no privilege boundary for the
  subprocess tier (it runs as the same OS user as the control
  plane).
- **Cryptographic-quality protection of LLM outputs.** The model
  may emit anything in any conversation, including content that
  *appears* to be commands or assertions. Our defense is
  architectural (the LLM's output is data, not control flow), not
  cryptographic — we do not "verify" model output beyond
  schema-validating tool-call args.
- **Side-channel attacks against the LLM.** Timing, token-count,
  cache-residency side channels are not defended against. The
  threat model assumes the LLM is a black-box oracle that the
  attacker can query freely.
- **Universal prompt-injection prevention.** Per
  [`docs/agent-model.md` §8](agent-model.md), CaMeL-style
  containment closes the highest-impact vector cheaply but is not
  a complete defense. See §5 below.
- **DoS at the network edge.** A determined attacker can flood
  `/api/webhooks/...` with traffic; the host doesn't ship rate
  limiting at the HTTP layer. Operators who expose execlaw to the
  public internet should put a reverse proxy in front.

---

## 3. Cryptography in use

| Purpose | Algorithm | Where |
|---|---|---|
| At-rest DB encryption | SQLCipher (AES-256-CBC + PBKDF2-HMAC-SHA512 KDF) | `crates/core` with `sqlcipher` feature |
| Vault master-key storage | OS keyring + file fallback at `~/.execlaw/master.key` | `crates/vault/src/keyring_key.rs` |
| Admin password | Argon2id (default params) | `crates/server/src/routes.rs` (`verify_password`) |
| Event-log integrity | Versioned HMAC-SHA256: independent v1 rows, chained v2 rows, signed terminal heads | `crates/core/src/event_hmac.rs`, `crates/core/src/events.rs` |
| Artifact content identity | SHA-256 for ZIPs, subprocesses, OCI references, and SBOM sidecars | `crates/core/src/artifact_provenance.rs` |
| Build provenance | Offline cosign verification of SLSA provenance against SQLite allowlists | `crates/core/src/artifact_provenance.rs`, `crates/server/src/bundled_plugins.rs` |
| Capability tokens (runner) | Ed25519 (EdDSA), short-lived JWTs | `crates/server/src/auth.rs` |
| Session tokens (SPA) | Ed25519 access JWT (15 min) + refresh JWT (7 d) | `crates/server/src/auth.rs` |
| Approval tokens (cold contact) | Ed25519 JWT with `jti = approval_id` | `crates/server/src/approvals.rs` |
| WebAuthn (second-factor) | Whatever the registered authenticator supports | `crates/server/src/webauthn.rs` |
| TLS to local inference | rustls (no system CA dep) | `crates/inference-api` |
| TLS to plugin endpoints | rustls | per-plugin via `reqwest` |

Keys are generated per-install. Event-integrity rotation adds a new key id for
future rows and checkpoints while retaining old keys for verification; it does
not re-sign prior ranges. Destructive event re-signing is rejected after any
v2 checkpoint exists. JWT signing-key rotation remains a separate operation.

There is no cloud HSM, no remote KMS, no key escrow. Keys are local.
`execlaw backup` writes a database snapshot, not a separately passphrase-wrapped
vault export. Its encryption depends on the SQLCipher-enabled path. Retain the
matching master key and event signer separately, following the
[recovery drill](key-rotation-drill.md).

---

## 4. Trust assumptions

### The operator (Controller principal)

**Fully trusted.** All capabilities, all tool access, all memory
scopes. The Controller's identity is bound cryptographically via a
WebAuthn credential and / or an Argon2id-hashed password.

### Other principals (contacts on bridged transports)

Trust is assigned per the trust ladder — see
[`docs/architecture.md` §5.4](architecture.md):

```
Controller > Delegated > KnownTrusted > KnownLimited > UnknownPending > Blocked
```

The `Blocked` state is universal — it applies to strangers AND to
previously-trusted principals the controller has revoked.
`UnknownPending` is the cold-contact entry state; the agent does not
run inference until the controller decides.

### Plugins

**Trusted code** — see the explicit non-defense in §2 above. Three
implications:

1. **Don't install plugins from sources you don't audit.** The script
   tier (Rhai) runs in the host process; the subprocess tier runs as
   the same OS user. There is no sandbox.
2. **Plugin manifest validation is structural, not behavioural.**
  The host parses the TOML and compiles declared tool schemas as JSON
  Schema Draft 2020-12 before registering any hooks. Missing, malformed,
  or externally referencing declared schemas reject registration; compiled
  schemas validate arguments before credential lookup and dispatch. The
  host does not analyze plugin code for intent.
3. **Plugin updates require an explicit upgrade mode**, but that is not an
   authorization boundary. The reviewed lifecycle handlers omit caller
   authentication (F01), and staging can replace files before provenance
   rejection (F02). Controller authorization and transactional replacement
   remain H026/H027 acceptance gates.

### MCP servers

**Untrusted, but firewalled.** MCP tools dispatch through a separate
client (`crates/mcp-client/`) that does not pass the caller's trust
class or capability set to the MCP server. MCP servers can return
arbitrary content; that content flows through the same spotlighting +
planner/executor containment as any other untrusted input. MCP
servers cannot mint outbox rows directly — they can only return
tool-call results that the policy engine still gates.

---

## 5. Known limitations

These are real and documented, not hypothetical.

### Prompt injection at the model level

Per Meta's "Agents Rule of Two" (and DeepMind's CaMeL paper, and the
2025 "The Attacker Moves Second" red-team study), there is no model-
level defense against prompt injection that survives motivated
adversaries. execlaw's posture is **architectural containment**:

- The Rule of Two policy gate stops any single turn from combining
  more than two of {ingests-untrusted, accesses-sensitive,
  produces-external-effect}.
- Untrusted-content turns route through a planner/executor split —
  the role with tools never sees the injection; the role that sees
  the injection has no tools.
- Spotlighting wraps untrusted text in randomized delimiters.
- Sideband HITL ensures the controller is notified via a *different*
  transport than the one carrying the untrusted content.

These narrow the blast radius. They do not eliminate the vector. If
your threat model includes "skilled adversary deliberately targeting
the operator," assume they will succeed at reaching the model and
plan accordingly (e.g. don't grant `Controller` trust to a contact
whose phone is shared with someone hostile).

### Windows OS keyring drift

Windows Credential Manager has documented issues with credential
loss across user-profile touch events and session-token rotations.
`crates/vault/src/keyring_key.rs` implements a defensive fallback:
the keyring is treated as a cache, the on-disk
`~/.execlaw/master.key` file is the durable sink. The fallback has
not been qualified here against the installed Windows service account.
The repository now has Windows CI, but default-feature unit tests are not
evidence of Credential Manager, key-file ACL, or service-identity behavior in
the packaged application. H028/H076/H117 require that platform evidence.

### Plugin sandboxing

Plugin runtimes remain operator-curated code; the script and subprocess tiers
are not general-purpose security sandboxes. Optional plugin UI panels run
separately in an opaque-origin iframe with scripts only, no parent DOM or
origin-storage access, and `connect-src 'none'`. Panel API calls cross a source- and
nonce-checked parent RPC broker. The broker permits only that panel's
manifest-declared plugin routes and narrow own-plugin settings, sidecar,
identity, or OAuth capabilities. Artifact verification establishes which
reviewed source/workflow produced an artifact and detects byte substitution;
it does not make plugin runtime code safe.

### Release provenance handoff

Packaging scripts generate SPDX 2.3 sidecars. Platform workflows then keylessly
create detached `.provenance.json` statements and offline `.sigstore.json` SLSA
bundles before desktop packaging. The exact ZIP and sidecars are embedded in
the desktop artifact and published together; bundled installation verifies
them offline against the Controller allowlists.

### CI and release evidence gaps

`.github/workflows/ci.yml` defines Linux x86_64, Windows MSVC, and Apple Silicon
macOS checks using plaintext SQLite defaults. It does not provide the SQLCipher
or Criterion qualification described by H028/H049. F04/F16 record those gaps;
the presence of a workflow is not evidence that a particular artifact passed.
Intel macOS is not in that supported CI matrix.

### Backup confidentiality depends on the artifact and keys

A SQLCipher snapshot plus its matching master key discloses database content.
A plaintext-feature snapshot has no equivalent at-rest protection. Neither
backup file proves who authorized its restoration. Protect snapshots and key
material separately, verify the actual artifact, and follow the
[key-rotation and recovery drill](key-rotation-drill.md). Whole-machine recovery
and trusted rollback detection remain H118/H063 work.

### Webhook routes are public endpoints

`[[webhook_routes]]` mounts at `/api/webhooks/{plugin_id}{path}`
without execlaw session JWT authentication. The host enforces a
manifest-declared query-token or HMAC-SHA256-header mode before
publishing or dispatching the request. A legacy route that omits an
`auth` declaration must validate caller identity in its plugin handler.
Audit both the route declaration and any handler fallback before
relying on a plugin webhook.

### Logs may contain sensitive data

`tracing` events are mirrored into `~/.execlaw/logs/*.jsonl` and the
`log_entries` SQLite table. Plugins occasionally include user-
content excerpts in log lines (the WhatsApp plugin's diagnostic logs
in the v0.1.x series did this during debugging). Plugin authors are
expected to redact, but enforcement is by convention. Treat the log
files as confidential.

---

## 6. Dangerous actions and approval semantics

The policy gate treats dangerous actions as combinations of risk
dimensions, not as a static list of tool names. A turn is evaluated
on whether it:

- ingests untrusted content,
- touches sensitive state, and/or
- produces external effects.

If a requested action crosses the Rule of Two threshold (or lands in a
trust class that cannot self-authorize), the host creates an approval
record and blocks execution until a Controller verdict is recorded.

Operator model:

1. Approve only the minimum scope needed for this one action.
2. Use a verified effect-free simulation for proposals. The current automation
   `dry_run`/Test run invokes live handlers (F18), so its name is not a safety
   guarantee; H126 separates simulation from authorized live execution.
3. Treat "external effect" as high risk even when content appears benign.
4. Use sideband confirmations for ambiguous requests.

Protocol guarantees:

- Approval responses require a signed approval token; approval id alone
  is not sufficient.
- Turn replay preserves the block/allow decision path in the event log.
- Tool pairing invariant (`tool_use` + `tool_result`) still applies when
  a turn is interrupted by approval waits.

For plugin authors, this means effectful tools should be designed to
halt cleanly before side effects when the host indicates approval is
required, then resume with explicit approval context.

---

## 7. Hardening checklist (operator)

If you're deploying execlaw on a machine that's network-reachable:

1. Bind the control plane to loopback only (`127.0.0.1:3031` is the
   default). Don't change to `0.0.0.0:...` without a reverse proxy
   in front — the Settings → General "bind every interface" path is
   for operators who already have TLS termination.
2. Put a reverse proxy (nginx / Caddy / Traefik) in front for TLS
   when exposing the SPA externally. WebAuthn requires HTTPS in any
   non-`localhost` setting.
3. Set up the OS service registration via `execlaw install` rather
   than running `execlaw serve` from a terminal — the service path
   restarts on crash, runs as the right user, and integrates with
   the OS log surface.
4. Lock down the `~/.execlaw/` directory to the service user only
   (`chmod 700 ~/.execlaw` on POSIX). It contains the SQLCipher
   database, the file-fallback master key, log files, and per-
   plugin sidecar volumes.
5. Audit installed plugins. Each one is trusted code in your
   process / your user account. The signal-cli, wuzapi, and similar
   sidecars are similarly trusted — pin to known-good image
   digests, not `:latest`, in production.
6. Rotate event-integrity keys by adding a new key id and retaining prior keys
  for verification. Do not re-sign historical v2 ranges; the core rejects
  destructive re-signing after a v2 checkpoint exists.
7. Back up the database and preserve its matching master key and event signer
   separately on the same cadence as other operator-critical state. Verify
   snapshot encryption with a qualified SQLCipher artifact before relying on
   it for offsite confidentiality; inventory plugin state and artifacts too.

### Discord bot token exposure

If a Discord token appeared in a log accessible outside the operator's
machine, treat it as compromised. Reset the token in the Discord Developer
Portal, then enter the replacement only through **Settings → Plugins →
Discord**. Remove or restrict copies of the exposed log, including any
public issue, CI artifact, paste, or log collector; do not paste the old
token into a bug report. Updating the plugin does not revoke the old token.

Discord plugin v0.2.1 returns only a fixed redaction marker from its config
and status routes, and does not forward upstream validation errors to the
admin response or logs. The host also suppresses raw plugin-admin handler
errors and authenticated HTTP response bodies, since either may reflect a
credential supplied in a request header. The bot token remains in the
plugin-scoped vault and is not supplied by repository defaults.
