# Backup, key rotation, and secret incident drill

## Implementation and qualification scope

The [implementation plan](implementation-plan.md) tracks the committed
H001-H154 work. This drill is the current local-database baseline for
[H020](llm-harness-roadmap.md#enhancement-020); its qualification remains
verification-blocked in the tracker. It does not prove that an installed
desktop artifact is encrypted or that whole-machine recovery succeeds.
[F04 and the other open findings](llm-harness-roadmap.md#review-findings-and-unresolved-verification)
remain authoritative. All three desktop build commands now explicitly enable
SQLCipher, and packaged `doctor` runs its disposable encryption, backup/restore,
and rekey recovery check. The actual native release workflows still need their
later runner results recorded for
[H028](llm-harness-roadmap.md#enhancement-028). Do not treat changing a build
feature as migration of an existing plaintext DB.

The committed extensions are [H058 power-loss durability](llm-harness-roadmap.md#enhancement-058),
[H063 snapshot rollback detection](llm-harness-roadmap.md#enhancement-063),
[H064 migration histories](llm-harness-roadmap.md#enhancement-064),
[H076 secret lifetime](llm-harness-roadmap.md#enhancement-076), and
[H118 replacement-host recovery](llm-harness-roadmap.md#enhancement-118).
Their evidence must include matching keys, plugin/sidecar state, artifacts,
external reauthorization, and reconciliation of pending effects before sends
resume. Existing database tests alone do not cover that inventory. The vault
tests passed in the 2026-09-29 verification session. Under WSL, the
SQLCipher-only core rotation drill passed, and the production-feature CLI passed
`doctor` plus a temporary migration/backup/restore/key-rotation sequence. The
full SQLCipher workspace suite was interrupted when the WSL utility VM returned
`Wsl/Service/E_UNEXPECTED` during test-binary compilation, before it returned a
final summary. Native installed-package workflow results remain unrun. A
Windows CLI test target also compiled, but Windows Application Control
blocked its test executable before it ran; the production CLI flows were
executed in WSL instead.

With SQLCipher enabled, execlaw stores encrypted SQLite data under its master
key and signs event rows under a separate event HMAC key. On first startup,
`~/.execlaw/event-hmac.key` is initialized from the existing master key so
existing installations retain their event chain. Later SQLCipher rotations
change `master.key` while leaving the event signer stable. Both files are
operator secrets; keep them in the same access-controlled recovery set as the
database backups. The files contain hex-encoded key bytes and are never meant
to be copied into logs or tickets.

OAuth client credentials, access/refresh tokens, authorization codes, CSRF
state, token grants, and provider error bodies redact their sensitive fields
from Rust `Debug` and error output. Provider error bodies are not echoed to
callback responses. The source includes regression tests using synthetic
marker values to ensure formatted diagnostics do not contain them. OAuth client
upsert coverage also checks that replacement credentials persist without
changing the original creation timestamp. The core/provider redaction tests and
vault test suite passed; the admin redaction test passed before its Cargo
command was interrupted while walking unrelated integration binaries.

## Disposable SQLCipher drill

The SQLCipher-only core drill creates a temporary encrypted database with a
synthetic vault secret, plugin row, and signed event. It checks that a
pre-rotation backup opens with the old key, rekeys the test database, verifies
the secret/plugin rows and event history with the retained signer, writes a
new event, and confirms the old backup stays independently readable. The
synthetic secret is asserted in memory and is not printed.

```powershell
cargo test -p execlaw-core --no-default-features --features sqlcipher encrypted_rotation_drill_preserves_backup_secrets_plugins_and_event_chain
```

The installed binary's `doctor` command now performs a disposable packaged
preflight as well: it creates an encrypted DB, rejects a wrong key, verifies
an encrypted pre-rotation backup after restore, rekeys the live probe DB,
rejects the old key, and reopens a restored post-rotation backup with the new
key. It uses fixed synthetic raw keys only in its temporary database and emits
no key material. The production-feature CLI's doctor preflight passed these
checks under WSL on 2026-09-29. The native installer workflows still need runner
results recorded under H028.

## Live SQLCipher rotation

Stop the execlaw service and confirm that the destination backup path does not
already exist. Build the crypto-enabled CLI and run:

```powershell
cargo build -p execlaw --features sqlcipher
execlaw rotate-keys --db "$HOME/.execlaw/execlaw.db" `
  --backup "$HOME/.execlaw/recovery-2026-09-27.db" `
  --i-understand-database-key-will-change
```

The command leaves an old-key recovery snapshot next to the requested backup
while it rekeys the database. It then writes and verifies a new-key snapshot,
durably replaces `master.key`, and refreshes the OS keyring cache. On Unix the
sibling-file rename is atomic. Windows writes the existing key file in place
because Windows file handles can prevent replacement; the retained old-key
snapshot and old keyring entry cover an interrupted write. The
temporary old-key snapshot is deleted only after the new key file is durable.
It never re-signs event history. Start execlaw only after the command reports
success, then keep the verified new-key backup and both key files in separate,
access-controlled recovery storage.

If the process stops before the master-key file is replaced, restore the
old-key snapshot over the database and continue using the old `master.key`.
If it stops after the master-key file is replaced, the requested new-key
snapshot is the recovery copy. If a failed command reports that rollback also
failed, do not start the service: preserve both snapshots and the current key
files, then recover from the snapshot whose key matches the durable master
key. Rotation requires the service to be stopped because SQLite and the
keyring-file transition are coordinated by this CLI process.

`execlaw backup` and `execlaw restore` validate SQLite integrity, execlaw
schema, and the full event HMAC chain when SQLCipher is enabled. A database
backup alone is not sufficient for disaster recovery: retain the matching
master key and `event-hmac.key` in a separate protected location. The CLI does
not export plaintext key material as part of a database backup.

## Secret compromise response

For a compromised plugin credential, revoke it at its issuing service first,
then issue a replacement and update the plugin's configured vault reference.
Record the plugin and secret name, but never paste the credential into logs,
shell history, or incident notes. Check recent transport/outbox activity and
admin audit records for unauthorized effects. A database restore can restore
the old credential, so reapply revocation and replacement after any restore.

For a suspected database-key disclosure, stop execlaw, contain access to the
host and backups, rotate affected external credentials, and use the SQLCipher
rotation procedure to move the live database to a fresh master key. A database
key change does not rotate plugin credentials; those must be revoked and
replaced separately. Preserve the old encrypted snapshot and key under
restricted incident access until the recovery and audit are complete.

For a suspected `event-hmac.key` disclosure, preserve a read-only copy of the
database, key file, and logs for investigation. The current runtime uses a
single loaded event HMAC key. The core `KeyRing` supports retaining prior
verification keys and selecting a new signing key, but `rotate-keys` rotates
the database key, not a persisted runtime event-key ring. Do not replace the
event key file as a substitute for a qualified signer migration. Do not run
`execlaw resign-events`: that
overwrites existing tags and removes their original tamper-evidence. Treat the
chain as potentially forgeable from the time of exposure and record that
integrity limitation in the incident report.

## Runtime resource admission scope

Managed model start admission compares declared `required_ram_mb` with live
available host RAM. NVIDIA `required_vram_mb` is compared with NVML free memory,
including automatic selection across NVIDIA devices. Unknown capacity,
disappeared devices, or incomplete readings decline the start and are retried
on later reconciliation. Declared VRAM requirements on unmonitored non-NVIDIA
devices fail closed until a live vendor probe is available. These checks gate
managed process starts; external endpoints are not controlled by the local
resource supervisor.
