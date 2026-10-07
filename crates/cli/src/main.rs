//! `execlaw` CLI.
//!
//! Bare-metal lifecycle (Phase 14 — replaces the Phase 0 docker-compose
//! wrappers):
//!
//! - `execlaw install`           one-shot: migrate + register + start
//! - `execlaw service install`   register with systemd / launchd / SCM
//! - `execlaw service start`     start the service
//! - `execlaw service stop`      stop it
//! - `execlaw service restart`   stop + start
//! - `execlaw service status`    print install state + per-OS log commands
//! - `execlaw service uninstall` deregister
//!
//! Other:
//!
//! - `execlaw doctor`            checks vault + db + (optional) Docker for
//!   managed-mode backends
//! - `execlaw db migrate`        run pending migrations
//! - `execlaw hw rescan`         (stub — §Phase 2)
//! - `execlaw serve`             run the server in foreground (dev / debug)
//!
//! Docker is only relevant for managed-mode backends now (Phase 12);
//! the control plane itself runs as a host service.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod api_client;
mod privacy_restore;
mod service;

// 2026-05-18 — process-wide python-sandbox service handle moved
// into `execlaw_server::python_sandbox::SERVICE` so request handlers
// (specifically the `DELETE /api/chats/{id}` handler that needs to
// call `on_conversation_deleted`) can reach it. The cli used to
// keep a local OnceLock here purely to keep the Arc alive across
// the spawned wiring task; that role is now subsumed by the
// server-crate static. See `python_sandbox::set_service` /
// `python_sandbox::service()`.

#[derive(Debug, Parser)]
#[command(name = "execlaw", version, about = "execlaw control plane CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// First-run bare-metal install: db migrate + register + start the
    /// service. The service uses systemd (Linux), launchd (macOS), or
    /// Windows Service Control Manager — see `execlaw service --help`.
    Install {
        /// Open the local DB plaintext during migrate (dev only).
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
        /// Install at system level (root / Administrator) instead of
        /// per-user. Required on Windows for SCM access; optional on
        /// Linux + macOS.
        #[arg(long, default_value_t = false)]
        system: bool,
        /// Skip db migrate (e.g. operator already ran it).
        #[arg(long, default_value_t = false)]
        skip_migrate: bool,
        /// Override the default bind address (loopback:3031).
        #[arg(long)]
        bind: Option<String>,
        /// Override the default DB path (`~/.execlaw/execlaw.db`).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Manage the long-running control-plane service.
    Service {
        #[command(subcommand)]
        op: ServiceOp,
    },
    /// Run preflight environment checks (DB, vault, optional Docker).
    Doctor,
    /// Database operations.
    Db {
        #[command(subcommand)]
        op: DbOp,
    },
    /// Hardware detection (stub for Phase 2).
    Hw {
        #[command(subcommand)]
        op: HwOp,
    },
    /// Use execlaw's versioned headless Controller API from a terminal.
    Client {
        #[command(subcommand)]
        op: ClientOp,
    },
    /// Qualify the configured local Standard backend using this machine's database.
    QualifyModel {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
        #[arg(long, default_value_t = 4096)]
        context_tokens: u32,
    },
    /// Run the HTTP server directly — for local dev / tests.
    ///
    /// Production uses `execlaw up` which spawns the container.
    Serve {
        /// Override the configured bind address for this run. When
        /// omitted, the value falls back to
        /// `config_general.bind_address` (set in Settings → General),
        /// then to the hardcoded `127.0.0.1:3031`. Passing `--bind`
        /// is intended for one-off dev runs; persistent changes
        /// should go through the SPA.
        #[arg(long)]
        bind: Option<String>,
        /// Database file. Defaults to `~/.execlaw/execlaw.db`.
        #[arg(long)]
        db: Option<PathBuf>,
        /// If set, open the DB plaintext (dev only).
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
        /// Permit unsigned local artifacts for this run. This is persisted as
        /// Controller policy and every use emits an audit record.
        #[arg(long, default_value_t = false)]
        allow_unsigned_local_development: bool,
    },
    /// Replay a turn — reconstructs the exact prompt the model saw,
    /// the policy decision (capabilities, planner_executor, etc.),
    /// and the events `commit_turn` produced for that turn.
    ///
    /// Used to debug "why did the model do that on turn 47?" without
    /// re-running inference.
    Replay {
        /// Conversation id.
        conversation_id: String,
        /// Inclusive upper-bound seq. Replay reconstructs state up
        /// to and including this seq.
        #[arg(long)]
        at: i64,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Eval-flag operations — tag event ranges as regression
    /// targets for the LLM-judge harness.
    Eval {
        #[command(subcommand)]
        op: EvalOp,
    },
    /// Governed memory assertion tools.
    Memory {
        #[command(subcommand)]
        op: MemoryOp,
    },
    /// Phase-7 hardening: scan `state_events` for rows with NULL
    /// `tag` and sign them under the current HMAC key. Idempotent.
    /// Run once per fleet before flipping the column to NOT NULL.
    BackfillEvents {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Recovery hatch: re-sign every `state_events` row under the
    /// current HMAC key, OVERWRITING the existing tags. Use when the
    /// keyring lost the original key and the operator has accepted
    /// that history is now signed under a new key (tamper-evidence
    /// for already-stored rows is destroyed).
    ResignEvents {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
        /// Required: confirms the operator understands this destroys
        /// the tamper-evidence guarantee on existing rows.
        #[arg(long, default_value_t = false)]
        i_understand_history_will_be_resigned: bool,
    },
    /// Phase-7 hardening: snapshot the SQLCipher DB to a destination
    /// path using `VACUUM INTO`. The destination is a self-contained
    /// SQLite file with the same encryption posture as the source.
    Backup {
        /// Output path. Parent directory must exist.
        #[arg(long)]
        to: PathBuf,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Rotate the SQLCipher master key after creating and verifying an
    /// encrypted recovery snapshot. Stop execlaw before running this command.
    RotateKeys {
        /// Required encrypted recovery snapshot path; must not already exist.
        #[arg(long)]
        backup: PathBuf,
        #[arg(long)]
        db: Option<PathBuf>,
        /// Required acknowledgement that the live database encryption key changes.
        #[arg(long, default_value_t = false)]
        i_understand_database_key_will_change: bool,
    },
    /// Phase-7 hardening: validate a snapshot file (must be openable
    /// with the same key + carry the migrations table) and atomically
    /// swap it into place. Refuses to overwrite a live DB without
    /// `--force`.
    Restore {
        /// Snapshot path produced by `execlaw backup`.
        #[arg(long)]
        from: PathBuf,
        /// Live DB path to replace.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Allow overwriting a non-empty target file.
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Permanently remove one conversation and all of its derived rows.
    /// Stop the control plane before invoking this recovery command.
    DeleteThread {
        /// Conversation id to remove.
        conversation_id: String,
        /// Required acknowledgement because this deletes event history.
        #[arg(long, default_value_t = false)]
        i_understand_this_deletes_history: bool,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// List conversation ids and their sidebar metadata.
    ListThreads {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ClientOp {
    /// Authenticate and save the rotated refresh token in the OS keyring.
    Login {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        username: String,
    },
    /// Run the Controller model qualification matrix and print its results.
    Qualify {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long, default_value_t = 4096)]
        context_tokens: u32,
    },
    /// Securely import an already-issued refresh token from a TTY prompt.
    ImportRefreshToken,
    /// Send a task and print the assistant response as JSON.
    Send {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        conversation_id: String,
        #[arg(long, required_unless_present = "resume_run_id", default_value = "")]
        text: String,
        /// Reuse this idempotency key when retrying an uncertain request.
        #[arg(long)]
        request_id: Option<String>,
        /// Resume this conversation's persisted run using its saved input.
        #[arg(long)]
        resume_run_id: Option<String>,
        /// Required acceptance check in ID=DESCRIPTION form. Repeat for each check.
        #[arg(long = "acceptance-criterion", action = clap::ArgAction::Append)]
        acceptance_criteria: Vec<String>,
        /// Optional acceptance check in ID=DESCRIPTION form; failure labels the task partial.
        #[arg(long = "optional-acceptance-criterion", action = clap::ArgAction::Append)]
        optional_acceptance_criteria: Vec<String>,
        /// Required artifact in ID=DESCRIPTION form. Repeat for each artifact.
        #[arg(long = "required-artifact", action = clap::ArgAction::Append)]
        required_artifacts: Vec<String>,
        /// Do not report the task complete until delivery evidence is confirmed.
        #[arg(long, default_value_t = false)]
        delivery_required: bool,
    },
    /// Read messages after a durable event cursor.
    Messages {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        conversation_id: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    /// Stop the active turn.
    Stop {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        conversation_id: String,
    },
    /// Persist and deliver steering, pause, resume, or queue-next-turn intent.
    Control {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        conversation_id: String,
        #[arg(long, value_parser = ["steer", "queue_next_turn", "pause", "resume", "cancel"])]
        kind: String,
        #[arg(long)]
        text: Option<String>,
        /// Reuse the same key when retrying an uncertain control delivery.
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Reconnect to durable control acknowledgements.
    Controls {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        conversation_id: String,
        #[arg(long, default_value_t = 0)]
        after_created_at: i64,
    },
    /// List pending Controller approvals.
    Approvals {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
    },
    /// Respond to an approval using its signed approval token.
    RespondApproval {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        approval_id: String,
        #[arg(long)]
        approval_token: String,
        #[arg(long, value_parser = ["approve", "edit", "reject", "trust", "trust_limited", "claim_as_me", "block", "ignore_once"])]
        verb: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Download an authenticated attachment/artifact to a local path.
    DownloadArtifact {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        attachment_id: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Register a Controller-selected local project directory.
    RegisterWorkspace {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        path: String,
    },
    /// Read a bounded UTF-8 file from a registered workspace root.
    WorkspaceRead {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        workspace_id: String,
        #[arg(long)]
        path: String,
    },
    /// Search bounded text files inside a registered workspace root.
    WorkspaceSearch {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        workspace_id: String,
        #[arg(long)]
        query: String,
    },
    /// Capture a content-addressed checkpoint into a durable run.
    WorkspaceCheckpoint {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        workspace_id: String,
    },
    /// Preview workspace edits and concurrent-file conflicts for a run.
    WorkspaceDiff {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        run_id: String,
    },
    /// Apply a conflict-free diff preview using an idempotency key.
    WorkspaceApply {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        preview_hash: String,
        #[arg(long)]
        request_id: String,
    },
    /// Restore only the latest applied run-owned file changes.
    WorkspaceRestore {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        apply_id: String,
        #[arg(long)]
        request_id: String,
    },
    /// Remove the stored API refresh token from the OS keyring.
    Logout,
    /// Run an allowlisted Language Server Protocol command adapter on stdio.
    EditorAdapter {
        #[arg(long, default_value = "http://127.0.0.1:3031")]
        server: String,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceOp {
    /// Register the service with the host's service manager.
    Install {
        #[arg(long, default_value_t = false)]
        system: bool,
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Start the service.
    Start {
        #[arg(long, default_value_t = false)]
        system: bool,
    },
    /// Stop the service.
    Stop {
        #[arg(long, default_value_t = false)]
        system: bool,
    },
    /// Stop, then start.
    Restart {
        #[arg(long, default_value_t = false)]
        system: bool,
    },
    /// Print install state + per-OS commands for live status / logs.
    Status {
        #[arg(long, default_value_t = false)]
        system: bool,
    },
    /// Deregister the service.
    Uninstall {
        #[arg(long, default_value_t = false)]
        system: bool,
    },
    /// Hidden — invoked by the service unit / SCM. Operators don't
    /// run this directly; `service install` registers it as the
    /// program path. Bind address is read from
    /// `config_general.bind_address` so SPA edits take effect on the
    /// next service restart without needing to rewrite the unit.
    #[command(hide = true)]
    Run {
        /// Hidden override for testing — production service units
        /// don't pass this; the binary reads bind from the DB.
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DbOp {
    /// Convert an offline plaintext SQLite database to SQLCipher in place.
    /// A separate verified plaintext backup is required before replacement.
    EncryptPlaintext {
        /// Plaintext database file to convert.
        #[arg(long)]
        db: PathBuf,
        /// New plaintext recovery snapshot. The path must not already exist.
        #[arg(long)]
        backup: PathBuf,
        /// Confirm execlaw is stopped and the live database can be replaced.
        #[arg(long, default_value_t = false)]
        i_understand_execlaw_is_stopped: bool,
    },
    /// Apply pending migrations.
    Migrate {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Print the current schema version.
    Status {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Re-stamp the stored checksum for an already-applied migration
    /// to match the embedded SQL. Use ONLY when the runner refuses
    /// with "migration id N already applied but with a different
    /// checksum" because of a benign byte-level edit (line endings,
    /// whitespace). Does not re-run the migration body — columns and
    /// tables stay put.
    RepairChecksum {
        /// Migration id to repair (e.g. `35`).
        #[arg(long)]
        id: u32,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HwOp {
    /// Re-run tier-1 sysfs detection.
    Rescan,
}

#[derive(Debug, Subcommand)]
enum EvalOp {
    /// Tag a range of events on a conversation as a regression target.
    Flag {
        /// Conversation id.
        conversation_id: String,
        /// Inclusive event seq range, e.g. `12..48`.
        #[arg(long)]
        range: String,
        /// Short human-readable label for the flag.
        #[arg(long)]
        label: String,
        /// Optional comma-separated tags (`trust-class,rule-of-two`).
        #[arg(long)]
        tags: Option<String>,
        /// Optional notes.
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// List eval flags. Filter by label if provided.
    List {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
    /// Export a flagged trajectory to a redacted, effects-disabled fixture.
    ExportFlagged {
        /// Eval flag id returned by `execlaw eval list`.
        id: i64,
        /// New fixture path. Existing files are never overwritten.
        #[arg(long)]
        to: PathBuf,
        /// Local JSON map: {"replacements":[{"source":"private","replacement":"<SYNTHETIC_1>"}]}.
        /// The map is read locally and is never copied into the fixture.
        #[arg(long)]
        redaction_map: PathBuf,
        /// Explicitly consent to exporting this flagged source range after redaction.
        #[arg(long, default_value_t = false)]
        consent: bool,
        /// Link the fixture to a bounded incident identifier, such as INC-42.
        #[arg(long)]
        incident_ref: Option<String>,
        /// Link the fixture to a bounded release tag, such as v2026.09.29.
        #[arg(long)]
        release_ref: Option<String>,
        /// Local exact catalog snapshots for flagged turns created before manifests stored declarations.
        #[arg(long)]
        tool_catalog_snapshots: Option<PathBuf>,
        /// Local JSON list of explicitly synthetic media replacements; source attachment bytes are never exported.
        #[arg(long)]
        synthetic_media: Option<PathBuf>,
        /// Local JSON list of policy inputs and expected decisions to replay against the production evaluator.
        #[arg(long)]
        policy_cases: Option<PathBuf>,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryOp {
    /// Export one assertion and verified evidence locally with explicit redaction consent.
    ExportAssertion {
        /// Stable assertion id from the Controller Memory Assets page.
        assertion_id: String,
        /// New destination path; existing files are never overwritten.
        #[arg(long)]
        to: PathBuf,
        /// Reviewed local JSON replacement map. It is not included in the export.
        #[arg(long)]
        redaction_map: PathBuf,
        /// Acknowledge local redaction review before writing a private-memory export.
        #[arg(long, default_value_t = false)]
        consent: bool,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        no_encrypt: bool,
    },
}

/// Tracing subscriber init — stdout (JSON or human-readable) plus
/// a daily-rotated JSONL file under `~/.execlaw/logs/` per §14.
///
/// File path is `<data_dir>/logs/execlaw.jsonl.YYYY-MM-DD`. The
/// returned `WorkerGuard` must be held for the lifetime of the
/// process — when it drops, the appender's background flush thread
/// shuts down and any unflushed lines are lost.
///
/// Set `EXECLAW_LOG_FORMAT=json` to get JSON on stdout too;
/// `EXECLAW_LOG_DIR` overrides the file directory; `EXECLAW_NO_FILE_LOG=1`
/// disables the file appender (useful for tests + ephemeral CLI
/// invocations like `execlaw doctor`).
/// Replace the default `eprintln!`-based panic hook with one that
/// emits a structured tracing event AND aborts the process. See
/// the call site comment for the rationale; mirrors the runner-
/// binary's `install_panic_hook` for journal-grep parity (target
/// `server::panic` on this side).
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("<non-string panic payload>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_owned());
        tracing::error!(
            target: "server::panic",
            message,
            location,
            backtrace = %backtrace,
            "SERVER_PANIC — process aborting (core dump if ulimit -c allows)"
        );
        std::process::abort();
    }));
}

fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Default filter: `info` for everything else, plus a per-crate
    // override that silences html5ever's noisy WARN spam. The
    // `html5ever::serialize` module fires `warn!("node with weird
    // namespace ...")` for every element with a non-html/mathml/svg
    // namespace — which fires constantly when dom_smoothie
    // serializes the cleaned DOM during deep-research gather (one
    // line per element per page; thousands per job). It's a known
    // upstream issue (servo/html5ever#122) that they've left as a
    // FIXME for years; safe to ignore at our level.
    //
    // Operators can still see those messages by setting
    // RUST_LOG="html5ever=warn,..." explicitly; the default just
    // gets them out of the way.
    let default_filter_directive =
        "info,html5ever=error,markup5ever=error,html5ever::serialize=error";
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter_directive));
    let want_json = std::env::var("EXECLAW_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    // Stdout layer.
    let stdout_layer = if want_json {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(std::io::stdout)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .boxed()
    };

    // File layer — daily-rotated JSONL.
    let (file_layer, guard) = match resolve_log_dir() {
        Some(log_dir) => {
            if let Err(e) = std::fs::create_dir_all(&log_dir) {
                eprintln!("execlaw: failed to create log dir {log_dir:?}: {e}");
                (None, None)
            } else {
                let file_appender = tracing_appender::rolling::daily(&log_dir, "execlaw.jsonl");
                let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
                let layer = tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(non_blocking)
                    .with_ansi(false)
                    .boxed();
                (Some(layer), Some(guard))
            }
        }
        None => (None, None),
    };

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer);
    let _ = match file_layer {
        Some(fl) => registry.with(fl).try_init(),
        None => registry.try_init(),
    };

    guard
}

/// Returns the directory the tracing file appender writes
/// `execlaw.jsonl.<DATE>` files into, or `None` if the operator has
/// disabled file logging (`EXECLAW_NO_FILE_LOG=1`). Same resolution
/// rules as `init_tracing` so both sides agree on which dir the log
/// viewer reads from.
pub(crate) fn resolve_log_dir() -> Option<PathBuf> {
    let want_file = std::env::var("EXECLAW_NO_FILE_LOG")
        .map(|v| !matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(true);
    if !want_file {
        return None;
    }
    let dir = std::env::var("EXECLAW_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_data_dir().join("logs"));
    Some(dir)
}

fn default_data_dir() -> PathBuf {
    // directories::ProjectDirs picks the right per-OS path. On Linux this
    // resolves to ~/.local/share/execlaw — but we document ~/.execlaw as
    // the conventional location, so prefer that.
    if let Some(home) = dirs_home() {
        home.join(".execlaw")
    } else {
        PathBuf::from(".execlaw")
    }
}

fn dirs_home() -> Option<PathBuf> {
    directories::UserDirs::new().map(|d| d.home_dir().to_path_buf())
}

fn default_db_path() -> PathBuf {
    default_data_dir().join("execlaw.db")
}

pub(crate) fn open_db(db_path: &Path, no_encrypt: bool) -> anyhow::Result<execlaw_core::Database> {
    let (db, _cfg) = open_db_with_config(db_path, no_encrypt)?;
    Ok(db)
}

/// Open the DB and ALSO return the `DbConfig` used to open it.
/// Production callers that need to do file-level lifecycle ops on
/// the DB (factory reset — close the connection, delete the file,
/// re-open) stash the config in `AppState::db_config` so they can
/// re-open at the same path with the same encryption posture
/// without re-querying the OS keyring.
pub(crate) fn open_db_with_config(
    db_path: &Path,
    no_encrypt: bool,
) -> anyhow::Result<(execlaw_core::Database, execlaw_core::DbConfig)> {
    use execlaw_core::db::SqlCipherKey;

    let key = if no_encrypt {
        None
    } else {
        let key_bytes = execlaw_vault::load_or_create_master_key()
            .map_err(|e| anyhow::anyhow!("could not load master key from keyring: {e}"))?;
        Some(SqlCipherKey::RawBytes(key_bytes.to_vec()))
    };
    let cfg = execlaw_core::DbConfig {
        path: db_path.to_path_buf(),
        key,
    };
    let db = execlaw_core::Database::open(&cfg)?;
    Ok((db, cfg))
}

/// Build the runner image when it's missing OR older than the
/// running control-plane binary. Operator workflow: bump source,
/// `cargo build`, restart the control plane — the next supervisor
/// boot rebuilds the runner image automatically without a manual
/// `docker build` step.
///
/// Locates the workspace by walking up from the current
/// executable looking for `Dockerfile.runner`. Production builds
/// that don't ship the source tree fall through silently — the
/// existing "image not present → warn + disable" path covers the
/// no-build-context case.
async fn ensure_runner_image_fresh(image: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let exe = std::env::current_exe().context("locate current_exe")?;
    let exe_mtime = std::fs::metadata(&exe)
        .and_then(|m| m.modified())
        .context("stat current_exe")?;

    let workspace = match find_workspace_with_dockerfile(&exe) {
        Some(p) => p,
        None => {
            tracing::debug!(
                "runner image autobuild skipped: no Dockerfile.runner found \
                 walking up from {}",
                exe.display(),
            );
            return Ok(());
        }
    };

    // Inspect the existing image. `docker image inspect <image>
    // --format {{.Created}}` returns ISO-8601 on hit, non-zero
    // exit on miss. We don't go through bollard here because the
    // path also needs `docker build` and that's only via the CLI.
    let inspect = std::process::Command::new("docker")
        .env_clear()
        .envs(execlaw_core::process_environment::current_minimal_environment())
        .args(["image", "inspect", "--format", "{{.Created}}", image])
        .output();
    let needs_build = match inspect {
        Ok(out) if out.status.success() => {
            let created_str = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            match chrono::DateTime::parse_from_rfc3339(&created_str) {
                Ok(image_dt) => {
                    let exe_dt: chrono::DateTime<chrono::Utc> = exe_mtime.into();
                    let stale = image_dt.with_timezone(&chrono::Utc) < exe_dt;
                    if stale {
                        tracing::info!(
                            image,
                            image_created = %image_dt,
                            exe_modified = %exe_dt,
                            "runner image is older than the control-plane binary; \
                             rebuilding",
                        );
                    } else {
                        tracing::debug!(
                            image,
                            image_created = %image_dt,
                            "runner image is up-to-date with the control-plane binary",
                        );
                    }
                    stale
                }
                Err(e) => {
                    tracing::warn!(
                        image,
                        raw = %created_str,
                        error = %e,
                        "couldn't parse `docker image inspect` Created field; rebuilding to be safe",
                    );
                    true
                }
            }
        }
        Ok(_) => {
            tracing::info!(
                image,
                "runner image not present locally; building from {}",
                workspace.display(),
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                image,
                error = %e,
                "could not run `docker image inspect`; assuming Docker is not \
                 available and skipping autobuild",
            );
            return Ok(());
        }
    };

    if !needs_build {
        return Ok(());
    }

    tracing::info!(
        image,
        workspace = %workspace.display(),
        "running `docker build -f Dockerfile.runner -t {image} .` (this may take a few minutes on first run)",
    );
    let status = std::process::Command::new("docker")
        .env_clear()
        .envs(execlaw_core::process_environment::current_minimal_environment())
        .arg("build")
        .arg("-f")
        .arg("Dockerfile.runner")
        .arg("-t")
        .arg(image)
        .arg(".")
        .current_dir(&workspace)
        .status();
    match status {
        Ok(s) if s.success() => {
            tracing::info!(image, "runner image build succeeded");
        }
        Ok(s) => {
            tracing::warn!(
                image,
                exit = ?s.code(),
                "runner image build returned non-zero exit; supervisor may use \
                 the previous image. Run the docker build manually to see the \
                 full output.",
            );
        }
        Err(e) => {
            tracing::warn!(
                image,
                error = %e,
                "could not invoke `docker build`; supervisor will fall back \
                 to the previous image (if any)",
            );
        }
    }
    Ok(())
}

/// Walk up from the executable looking for `Dockerfile.runner`.
/// Returns the workspace root (containing the Dockerfile) on hit.
/// `None` for production builds that ship without source.
fn find_workspace_with_dockerfile(exe: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = exe.parent();
    while let Some(dir) = cur {
        if dir.join("Dockerfile.runner").is_file() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

fn cmd_install(
    no_encrypt: bool,
    system: bool,
    skip_migrate: bool,
    bind: Option<String>,
    db: Option<PathBuf>,
) -> anyhow::Result<()> {
    println!("==> execlaw install (bare-metal)");

    // 1. Make sure the data dir exists. The vault + keyring + DB
    //    paths all live under it; the service unit also points its
    //    working_directory at it.
    let data_dir = default_data_dir();
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)?;
        println!("--> created {}", data_dir.display());
    }

    // 2. Migrate the local SQLite (encrypted by default; --no-encrypt
    //    for dev plaintext mode).
    if !skip_migrate {
        let db_path = db.clone().unwrap_or_else(default_db_path);
        println!("--> db migrate ({})", db_path.display());
        cmd_db_migrate(db_path, no_encrypt)?;
    } else {
        println!("--  skipping db migrate (--skip-migrate)");
    }

    // 3. Register the service with systemd / launchd / Windows SCM.
    println!(
        "--> service install ({} level)",
        if system { "system" } else { "user" }
    );
    service::install(system, bind, db)?;

    // 4. Start it.
    println!("--> service start");
    service::start(system)?;

    println!(
        "==> install complete — verify with `curl http://{}/api/health`",
        service::SERVICE_BIND
    );
    println!("    Use `execlaw service status` for live state + log paths.");
    Ok(())
}

fn cmd_doctor() -> anyhow::Result<()> {
    let mut ok = true;
    let mut report = String::new();

    // 1. Docker — optional now (Phase 14). The control plane runs as
    //    a host service; Docker is only needed for managed-mode
    //    backends (Phase 12) where the supervisor spawns container
    //    sidecars. A missing Docker downgrades to a NOTE not a
    //    failure.
    match std::process::Command::new("docker")
        .env_clear()
        .envs(execlaw_core::process_environment::current_minimal_environment())
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            report.push_str(&format!(
                "OK   docker:   {} (managed-mode backends available)",
                String::from_utf8_lossy(&out.stdout).trim()
            ));
            report.push('\n');
        }
        _ => {
            report.push_str(
                "NOTE docker:   not found — managed-mode backends disabled. \
                 External backends (operator-supplied URLs) still work.\n",
            );
        }
    }

    // 2. Data dir.
    let data_dir = default_data_dir();
    match std::fs::create_dir_all(&data_dir) {
        Ok(_) => {
            report.push_str(&format!("OK  data dir: {}\n", data_dir.display()));
        }
        Err(e) => {
            ok = false;
            report.push_str(&format!(
                "MISS data dir: can't create {}: {e}\n",
                data_dir.display()
            ));
        }
    }

    // 3. SQLCipher sanity — open a throwaway encrypted DB in a temp
    //    location. If SQLCipher isn't bundled correctly this fails.
    #[cfg(feature = "sqlcipher")]
    {
        let tmp = std::env::temp_dir().join(format!(
            "execlaw-doctor-sqlcipher-{}.db",
            uuid::Uuid::new_v4()
        ));
        let pre_rotation_backup = tmp.with_extension("pre-rotation.db");
        let restored_pre_rotation = tmp.with_extension("restored-pre-rotation.db");
        let post_rotation_backup = tmp.with_extension("post-rotation.db");
        let restored_post_rotation = tmp.with_extension("restored-post-rotation.db");
        let old_key = [0x31_u8; 32];
        let new_key = [0x73_u8; 32];
        let cfg = execlaw_core::DbConfig {
            path: tmp.clone(),
            key: Some(execlaw_core::db::SqlCipherKey::RawBytes(old_key.to_vec())),
        };
        let cipher_check = (|| -> Result<(), String> {
            let db = execlaw_core::Database::open(&cfg).map_err(|error| error.to_string())?;
            execlaw_core::MigrationRunner::new(&db)
                .apply_all()
                .map_err(|error| error.to_string())?;
            db.with_conn(|connection| {
                connection.execute_batch("CREATE TABLE doctor_probe(value TEXT);")?;
                connection.execute("INSERT INTO doctor_probe(value) VALUES ('cipher-ok')", [])?;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
            drop(db);

            let bytes = std::fs::read(&tmp).map_err(|error| error.to_string())?;
            if bytes.starts_with(b"SQLite format 3\0") {
                return Err("database header is plaintext SQLite".into());
            }

            let wrong_key = execlaw_core::DbConfig {
                path: tmp.clone(),
                key: Some(execlaw_core::db::SqlCipherKey::RawBytes(vec![0x42; 32])),
            };
            if let Ok(wrong_db) = execlaw_core::Database::open(&wrong_key) {
                let readable = wrong_db.with_conn(|connection| {
                    connection.query_row("SELECT COUNT(*) FROM schema_version", [], |row| {
                        row.get::<_, i64>(0)
                    })?;
                    Ok(())
                });
                if readable.is_ok() {
                    return Err("database accepted an incorrect key".into());
                }
            }

            let reopened = execlaw_core::Database::open(&cfg)
                .map_err(|error| format!("reopen with correct key: {error}"))?;
            reopened
                .with_conn(|connection| {
                    let migrations: i64 =
                        connection.query_row("SELECT COUNT(*) FROM schema_version", [], |row| {
                            row.get(0)
                        })?;
                    if migrations == 0 {
                        return Err(execlaw_core::DbError::Config(
                            "encrypted migration probe contains no migration rows".into(),
                        ));
                    }
                    let value: String =
                        connection
                            .query_row("SELECT value FROM doctor_probe", [], |row| row.get(0))?;
                    if value != "cipher-ok" {
                        return Err(execlaw_core::DbError::Config(
                            "encrypted data did not survive reopen".into(),
                        ));
                    }
                    Ok(())
                })
                .map_err(|error| error.to_string())?;

            // Exercise the same encrypted snapshot and rekey primitives used
            // by backup, restore, and rotation before a release package ships.
            write_backup_file(&reopened, &pre_rotation_backup)
                .map_err(|error| format!("create pre-rotation snapshot: {error}"))?;
            verify_database_snapshot_with_keys(&pre_rotation_backup, &old_key, &[0x55; 32])
                .map_err(|error| format!("verify pre-rotation snapshot: {error}"))?;
            std::fs::copy(&pre_rotation_backup, &restored_pre_rotation)
                .map_err(|error| format!("restore pre-rotation snapshot: {error}"))?;
            let restored_old = execlaw_core::Database::open(&execlaw_core::DbConfig {
                path: restored_pre_rotation.clone(),
                key: Some(execlaw_core::db::SqlCipherKey::RawBytes(old_key.to_vec())),
            })
            .map_err(|error| format!("open restored old-key snapshot: {error}"))?;
            let restored_value: String = restored_old
                .with_conn(|connection| {
                    Ok(connection
                        .query_row("SELECT value FROM doctor_probe", [], |row| row.get(0))?)
                })
                .map_err(|error| format!("read restored old-key snapshot: {error}"))?;
            if restored_value != "cipher-ok" {
                return Err("restored backup did not retain its encrypted probe row".into());
            }
            drop(restored_old);

            reopened
                .rekey_sqlcipher(&new_key)
                .map_err(|error| format!("rotate disposable SQLCipher key: {error}"))?;
            drop(reopened);
            let stale_key = execlaw_core::DbConfig {
                path: tmp.clone(),
                key: Some(execlaw_core::db::SqlCipherKey::RawBytes(old_key.to_vec())),
            };
            if let Ok(stale_db) = execlaw_core::Database::open(&stale_key) {
                let stale_key_readable = stale_db.with_conn(|connection| {
                    Ok(
                        connection.query_row("SELECT value FROM doctor_probe", [], |row| {
                            row.get::<_, String>(0)
                        })?,
                    )
                });
                if stale_key_readable.is_ok() {
                    return Err("rekeyed database still accepts the pre-rotation key".into());
                }
            }
            let rotated = execlaw_core::Database::open(&execlaw_core::DbConfig {
                path: tmp.clone(),
                key: Some(execlaw_core::db::SqlCipherKey::RawBytes(new_key.to_vec())),
            })
            .map_err(|error| format!("reopen after disposable key rotation: {error}"))?;
            let rotated_value: String = rotated
                .with_conn(|connection| {
                    Ok(connection
                        .query_row("SELECT value FROM doctor_probe", [], |row| row.get(0))?)
                })
                .map_err(|error| format!("read after disposable key rotation: {error}"))?;
            if rotated_value != "cipher-ok" {
                return Err("key rotation did not preserve the encrypted probe row".into());
            }
            write_backup_file(&rotated, &post_rotation_backup)
                .map_err(|error| format!("create post-rotation snapshot: {error}"))?;
            verify_database_snapshot_with_keys(&post_rotation_backup, &new_key, &[0x55; 32])
                .map_err(|error| format!("verify post-rotation snapshot: {error}"))?;
            drop(rotated);
            std::fs::copy(&post_rotation_backup, &restored_post_rotation)
                .map_err(|error| format!("restore post-rotation snapshot: {error}"))?;
            let restored_new = execlaw_core::Database::open(&execlaw_core::DbConfig {
                path: restored_post_rotation.clone(),
                key: Some(execlaw_core::db::SqlCipherKey::RawBytes(new_key.to_vec())),
            })
            .map_err(|error| format!("open restored new-key snapshot: {error}"))?;
            let restored_value: String = restored_new
                .with_conn(|connection| {
                    Ok(connection
                        .query_row("SELECT value FROM doctor_probe", [], |row| row.get(0))?)
                })
                .map_err(|error| format!("read restored new-key snapshot: {error}"))?;
            if restored_value != "cipher-ok" {
                return Err("restored rotated snapshot did not retain its probe row".into());
            }
            Ok(())
        })();
        match cipher_check {
            Ok(()) => report.push_str(
                "OK  sqlcipher: encrypted header, wrong-key rejection, migration, backup restore, rekey, and post-rotation restore verified\n",
            ),
            Err(error) => {
                ok = false;
                report.push_str(&format!("MISS sqlcipher: {error}\n"));
            }
        }
        for path in [
            &tmp,
            &pre_rotation_backup,
            &restored_pre_rotation,
            &post_rotation_backup,
            &restored_post_rotation,
        ] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(path.with_extension("db-wal"));
            let _ = std::fs::remove_file(path.with_extension("db-shm"));
        }
    }
    #[cfg(not(feature = "sqlcipher"))]
    {
        ok &= false;
        report.push_str("MISS sqlcipher: this binary was built without the sqlcipher feature\n");
    }

    // 4. Keyring — try to create/read a test entry.
    match keyring::Entry::new("execlaw", "doctor_probe") {
        Ok(entry) => {
            let _ = entry.set_password("ok");
            match entry.get_password() {
                Ok(_) => {
                    let _ = entry.delete_credential();
                    report.push_str("OK  keyring:  OS keyring reachable\n");
                }
                Err(e) => {
                    // This is only a warning — headless hosts fall back
                    // to a passphrase file.
                    report.push_str(&format!(
                        "WARN keyring: OS keyring not usable ({e}); passphrase fallback required\n"
                    ));
                }
            }
        }
        Err(e) => {
            report.push_str(&format!("WARN keyring: {e}\n"));
        }
    }

    println!("execlaw doctor\n--------------\n{report}");
    if ok {
        println!("verdict: OK");
        Ok(())
    } else {
        anyhow::bail!("doctor found blocking issues");
    }
}

fn cmd_db_migrate(db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    let db = open_db(&db_path, no_encrypt)?;
    let applied = execlaw_core::MigrationRunner::new(&db).apply_all()?;
    if applied.is_empty() {
        println!("nothing to apply; schema is up to date");
    } else {
        println!("applied migrations: {applied:?}");
    }
    Ok(())
}

fn cmd_db_encrypt_plaintext(
    db_path: PathBuf,
    backup_path: PathBuf,
    confirmed_stopped: bool,
) -> anyhow::Result<()> {
    if !confirmed_stopped {
        anyhow::bail!(
            "refusing to replace the database while execlaw may be running; stop it and pass \
             --i-understand-execlaw-is-stopped"
        );
    }

    #[cfg(not(feature = "sqlcipher"))]
    {
        let _ = (db_path, backup_path);
        anyhow::bail!("plaintext conversion requires an execlaw binary built with SQLCipher");
    }

    #[cfg(feature = "sqlcipher")]
    {
        let key_path = execlaw_vault::keyring_key::default_passphrase_file_path();
        anyhow::ensure!(
            key_path.is_file(),
            "durable master key file is missing at {}; restore the matching key before conversion",
            key_path.display()
        );
        let key = execlaw_vault::load_or_create_master_key()
            .map_err(|error| anyhow::anyhow!("load existing master key: {error}"))?;
        encrypt_plaintext_database(&db_path, &backup_path, &key)?;

        println!("encrypted database installed at {}", db_path.display());
        println!(
            "verified plaintext recovery backup at {}",
            backup_path.display()
        );
        Ok(())
    }
}

#[cfg(feature = "sqlcipher")]
fn encrypt_plaintext_database(
    db_path: &std::path::Path,
    backup_path: &std::path::Path,
    key: &[u8; 32],
) -> anyhow::Result<()> {
    use std::io::Read;

    use execlaw_core::db::SqlCipherKey;

    fn sql_literal(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn integrity_and_schema_count(db: &execlaw_core::Database) -> anyhow::Result<i64> {
        db.with_conn(|connection| {
            let integrity: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(execlaw_core::DbError::Config(format!(
                    "integrity_check failed: {integrity}"
                )));
            }
            let count = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type IN ('table', 'index', 'view', 'trigger') \
                   AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .map_err(Into::into)
    }

    anyhow::ensure!(
        db_path.is_file(),
        "database does not exist or is not a regular file: {}",
        db_path.display()
    );
    anyhow::ensure!(
        !backup_path.exists(),
        "backup path already exists; choose a new path: {}",
        backup_path.display()
    );
    let backup_parent = backup_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    anyhow::ensure!(
        backup_parent.is_dir(),
        "backup directory does not exist: {}",
        backup_parent.display()
    );

    let mut header = [0_u8; 16];
    std::fs::File::open(db_path)?.read_exact(&mut header)?;
    anyhow::ensure!(
        &header == b"SQLite format 3\0",
        "database is not plaintext SQLite; refusing conversion. It may already be encrypted, use a different key, or be damaged"
    );

    let original_permissions = std::fs::metadata(db_path)?.permissions();
    let mut temporary_name = db_path.as_os_str().to_os_string();
    temporary_name.push(format!(".sqlcipher-{}.tmp", uuid::Uuid::new_v4()));
    let encrypted_path = PathBuf::from(temporary_name);
    anyhow::ensure!(
        !encrypted_path.exists(),
        "temporary output already exists: {}",
        encrypted_path.display()
    );

    let conversion = (|| -> anyhow::Result<()> {
        let source = execlaw_core::Database::open(&execlaw_core::DbConfig {
            path: db_path.to_path_buf(),
            key: None,
        })
        .map_err(|error| anyhow::anyhow!("open plaintext source: {error}"))?;

        let source_schema_count = source
            .with_conn(|connection| {
                let integrity: String =
                    connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
                if integrity != "ok" {
                    return Err(execlaw_core::DbError::Config(format!(
                        "source integrity_check failed: {integrity}"
                    )));
                }
                let schema_count = connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type IN ('table', 'index', 'view', 'trigger') \
                   AND name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;

                // VACUUM INTO captures committed pages, including any WAL content,
                // into a standalone recovery copy before replacing the source.
                connection.execute(
                    &format!(
                        "VACUUM INTO {}",
                        sql_literal(&backup_path.to_string_lossy())
                    ),
                    [],
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(backup_path, std::fs::Permissions::from_mode(0o600))
                        .map_err(execlaw_core::DbError::from)?;
                }

                let target = sql_literal(&encrypted_path.to_string_lossy());
                let key_hex = hex::encode(key);
                connection.execute_batch(&format!(
                    "ATTACH DATABASE {target} AS encrypted KEY \"x'{key_hex}'\";"
                ))?;
                connection.execute_batch("SELECT sqlcipher_export('encrypted');")?;
                connection.execute_batch("DETACH DATABASE encrypted;")?;
                Ok(schema_count)
            })
            .map_err(|error| anyhow::anyhow!("snapshot and SQLCipher export: {error}"))?;
        drop(source);

        let plaintext_backup = execlaw_core::Database::open(&execlaw_core::DbConfig {
            path: backup_path.to_path_buf(),
            key: None,
        })
        .map_err(|error| anyhow::anyhow!("open plaintext recovery snapshot: {error}"))?;
        let backup_schema_count = integrity_and_schema_count(&plaintext_backup)?;
        drop(plaintext_backup);
        anyhow::ensure!(
            backup_schema_count == source_schema_count,
            "plaintext backup schema differs from source ({backup_schema_count} vs {source_schema_count})"
        );
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(backup_path)
            .map_err(|error| anyhow::anyhow!("open recovery snapshot for sync: {error}"))?
            .sync_all()
            .map_err(|error| anyhow::anyhow!("sync recovery snapshot: {error}"))?;

        let encrypted = execlaw_core::Database::open(&execlaw_core::DbConfig {
            path: encrypted_path.clone(),
            key: Some(SqlCipherKey::RawBytes(key.to_vec())),
        })
        .map_err(|error| anyhow::anyhow!("open encrypted staging database: {error}"))?;
        let encrypted_schema_count = integrity_and_schema_count(&encrypted)?;
        anyhow::ensure!(
            encrypted_schema_count == source_schema_count,
            "encrypted output schema differs from source ({encrypted_schema_count} vs {source_schema_count})"
        );
        encrypted.with_conn(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })?;
        drop(encrypted);

        let mut encrypted_header = [0_u8; 16];
        std::fs::File::open(&encrypted_path)?.read_exact(&mut encrypted_header)?;
        anyhow::ensure!(
            &encrypted_header != b"SQLite format 3\0",
            "encrypted output has a plaintext SQLite header"
        );
        std::fs::set_permissions(&encrypted_path, original_permissions)
            .map_err(|error| anyhow::anyhow!("preserve database permissions: {error}"))?;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&encrypted_path)
            .map_err(|error| anyhow::anyhow!("open encrypted staging file for sync: {error}"))?
            .sync_all()
            .map_err(|error| anyhow::anyhow!("sync encrypted staging file: {error}"))?;

        // WAL and shared-memory files belong to the plaintext source and
        // staging DBs; none may be replayed beside the replacement file.
        for database_path in [db_path, encrypted_path.as_path()] {
            for suffix in ["-wal", "-shm"] {
                let mut sidecar_name = database_path.as_os_str().to_os_string();
                sidecar_name.push(suffix);
                let sidecar = PathBuf::from(sidecar_name);
                match std::fs::remove_file(&sidecar) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(anyhow::anyhow!(
                            "remove stale SQLite sidecar {}: {error}",
                            sidecar.display()
                        ));
                    }
                }
            }
        }

        // The verified backup is already durable. POSIX rename replaces the
        // stopped service's database atomically. Windows needs a reversible
        // two-rename sequence because std::fs::rename cannot replace a file.
        #[cfg(not(windows))]
        std::fs::rename(&encrypted_path, db_path)?;
        #[cfg(windows)]
        {
            let mut displaced_name = db_path.as_os_str().to_os_string();
            displaced_name.push(format!(".plaintext-{}.old", uuid::Uuid::new_v4()));
            let displaced = PathBuf::from(displaced_name);
            std::fs::rename(db_path, &displaced)?;
            if let Err(error) = std::fs::rename(&encrypted_path, db_path) {
                let _ = std::fs::rename(&displaced, db_path);
                return Err(error.into());
            }
            std::fs::remove_file(displaced)?;
        }
        if let Some(parent) = db_path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    })();

    if conversion.is_err() {
        let _ = std::fs::remove_file(&encrypted_path);
        let _ = std::fs::remove_file(encrypted_path.with_extension("tmp-wal"));
        let _ = std::fs::remove_file(encrypted_path.with_extension("tmp-shm"));
    }
    conversion
}

fn cmd_db_status(db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    let db = open_db(&db_path, no_encrypt)?;
    let count = execlaw_core::MigrationRunner::new(&db).applied_count()?;
    println!("applied migrations: {count}");
    Ok(())
}

fn cmd_qualify_model(
    db_path: PathBuf,
    no_encrypt: bool,
    context_tokens: u32,
) -> anyhow::Result<()> {
    let db = open_db(&db_path, no_encrypt)?;
    let resolver = execlaw_server::inference_resolver::InferenceResolver::new(None);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime
        .block_on(execlaw_server::inference_probe::qualify_local_model(
            &db,
            &resolver,
            context_tokens,
        ))
        .map_err(|error| anyhow::anyhow!("{}: {}", error.code, error.message))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    anyhow::ensure!(
        result.qualified,
        "model qualification failed; see result matrix above"
    );
    Ok(())
}

fn cmd_db_repair_checksum(id: u32, db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    let db = open_db(&db_path, no_encrypt)?;
    let runner = execlaw_core::MigrationRunner::new(&db);
    let patched = runner.repair_checksum(id)?;
    if patched {
        println!(
            "repaired stored checksum for migration id {id} \
             to match embedded SQL on disk"
        );
    } else {
        println!(
            "no schema_version row for migration id {id}; nothing to repair \
             (run `execlaw db migrate` first if this is a fresh DB)"
        );
    }
    Ok(())
}

fn cmd_delete_thread(
    conversation_id: String,
    confirmed: bool,
    db_path: PathBuf,
    no_encrypt: bool,
) -> anyhow::Result<()> {
    if !confirmed {
        anyhow::bail!(
            "refusing to delete conversation history; pass \
             --i-understand-this-deletes-history"
        );
    }

    use execlaw_core::conversation::ConversationStore;
    use execlaw_core::ids::ConversationId;

    let db = open_db(&db_path, no_encrypt)?;
    let cid = ConversationId::from(conversation_id.as_str());
    let store = ConversationStore::new(&db);
    let existed = store.get(&cid)?.is_some();
    store.delete(&cid)?;
    println!(
        "conversation {} {}",
        cid.as_str(),
        if existed {
            "deleted"
        } else {
            "was already absent"
        }
    );
    Ok(())
}

fn cmd_list_threads(db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    use execlaw_core::conversation::ConversationStore;

    let db = open_db(&db_path, no_encrypt)?;
    for thread in ConversationStore::new(&db).list_thread_summaries()? {
        println!(
            "{}\t{}\t{}",
            thread.conversation_id.as_str(),
            thread.display_name.as_deref().unwrap_or("(unnamed)"),
            thread.last_activity_at,
        );
    }
    Ok(())
}

fn cmd_hw_rescan() -> anyhow::Result<()> {
    let profile = execlaw_container_manager::detect_sysfs(Path::new("/sys"));
    println!("{}", serde_json::to_string_pretty(&profile)?);
    Ok(())
}

/// `execlaw replay <conv_id> --at <seq>` — reconstruct the prompt
/// the model saw, the policy decision, and the events that turn
/// committed. Pure read-only operation against the SQLite log.
fn cmd_replay(
    conversation_id: String,
    at: i64,
    db_path: PathBuf,
    no_encrypt: bool,
) -> anyhow::Result<()> {
    use execlaw_core::events::{EventKind, EventLog};
    use execlaw_core::ids::{ConversationId, EventSeq};
    use execlaw_core::principal::PrincipalStore;
    use execlaw_core::principal::TrustLevel as CoreTrust;
    use execlaw_core::runs::RunStore;
    use execlaw_policy::trust::{TrustLevel, TurnPolicyInput, evaluate_turn};

    let db = open_db(&db_path, no_encrypt)?;
    let cid = ConversationId::from(conversation_id.as_str());
    let log = EventLog::new(&db);

    let all_events = log
        .replay_since(&cid, EventSeq(0))
        .map_err(|e| anyhow::anyhow!("replay: {e}"))?;
    if all_events.is_empty() {
        anyhow::bail!("no events for conversation {conversation_id}");
    }
    let target_seq = at;
    let target_idx = all_events
        .iter()
        .position(|e| e.seq.0 == target_seq)
        .ok_or_else(|| anyhow::anyhow!("seq {target_seq} not in conversation {conversation_id}"))?;

    // Walk backwards from target_seq to find the user_msg that
    // started this turn — replay reconstructs the turn that
    // CONTAINS the target seq.
    let mut user_msg_idx = target_idx;
    while user_msg_idx > 0 && all_events[user_msg_idx].kind != EventKind::UserMsg {
        user_msg_idx -= 1;
    }

    // Resolve sender trust at replay time. Prefer the persisted
    // PrincipalStore row (post-trust-changes); fall back to the
    // event's actor field for ephemeral senders.
    let actor = all_events[user_msg_idx]
        .actor
        .as_deref()
        .unwrap_or("controller");
    let sender_trust = if actor == "controller" {
        TrustLevel::Controller
    } else {
        let store = PrincipalStore::new(&db);
        match store.get(&execlaw_core::ids::PrincipalId::from(actor)) {
            Ok(Some(p)) => {
                TrustLevel::parse(p.trust_level.class_tag()).unwrap_or(TrustLevel::UnknownPending)
            }
            _ => {
                // Stamp at replay time as if we were resolving fresh.
                let _ = CoreTrust::Controller;
                TrustLevel::UnknownPending
            }
        }
    };

    let policy = evaluate_turn(TurnPolicyInput {
        effective_trust: sender_trust,
        sender_trust,
        voice: false,
        accesses_sensitive_data: false,
        produces_external_effect: false,
    });

    // Print the reconstructed turn.
    println!("=== Replay {conversation_id} @ seq {target_seq} ===");
    println!();
    println!("Sender trust:        {:?}", sender_trust);
    println!("Policy decision:");
    println!("  drop_turn:         {}", policy.drop_turn);
    println!("  require_approval:  {}", policy.require_approval);
    println!("  planner_executor:  {}", policy.planner_executor);
    println!("  spotlighting:      {}", policy.spotlighting);
    println!("  latency_band:      {:?}", policy.latency_band);
    println!("  capability_set:    {:?}", policy.capability_set);
    let run_store = RunStore::new(&db);
    if let Some(run) =
        run_store.find_run_for_input(&cid, EventSeq(all_events[user_msg_idx].seq.0))?
    {
        if let Some(inputs) = run_store.input_manifest(&run.run_id)? {
            println!("Turn input manifest (v{}):", inputs.input_version);
            println!("  prompt_hash:       {}", inputs.prompt_hash);
            println!("  model_hash:        {}", inputs.model_settings_hash);
            println!("  tool_catalog_hash: {}", inputs.tool_catalog_hash);
        } else {
            println!("Turn input manifest: unavailable (legacy run)");
        }
    } else {
        println!("Turn input manifest: unavailable (no durable run record)");
    }
    println!();
    println!("Reconstructed prompt history:");
    for ev in &all_events[..=user_msg_idx] {
        match ev.kind {
            EventKind::UserMsg => {
                let text = ev
                    .decode_payload::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(|s| s.to_owned()))
                    .unwrap_or_else(|| "<unparseable>".into());
                println!("  user[{}]: {text}", ev.seq.0);
            }
            EventKind::ModelTurn => {
                let text = ev
                    .decode_payload::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(|s| s.to_owned()))
                    .unwrap_or_else(|| "<unparseable>".into());
                println!("  assistant[{}]: {text}", ev.seq.0);
            }
            _ => {}
        }
    }
    println!();
    println!(
        "Events committed by/around the target turn (seq {} → {}):",
        all_events[user_msg_idx].seq.0, target_seq,
    );
    for ev in &all_events[user_msg_idx..=target_idx] {
        println!(
            "  seq={:>4}  kind={:<22}  actor={:?}",
            ev.seq.0,
            ev.kind.as_str(),
            ev.actor
        );
    }
    Ok(())
}

/// `execlaw eval flag <conv_id> --range a..b --label X` — record an
/// eval-flag row.
fn cmd_eval_flag(
    conversation_id: String,
    range: String,
    label: String,
    tags: Option<String>,
    notes: Option<String>,
    db_path: PathBuf,
    no_encrypt: bool,
) -> anyhow::Result<()> {
    use execlaw_core::eval::{EvalFlagRow, EvalFlaggedStore};
    use execlaw_core::ids::ConversationId;

    let (from, to) = parse_range(&range)?;
    let tags_vec: Vec<String> = tags
        .map(|s| s.split(',').map(|t| t.trim().to_owned()).collect())
        .unwrap_or_default();

    let db = open_db(&db_path, no_encrypt)?;
    let store = EvalFlaggedStore::new(&db);
    let id = store
        .insert(&EvalFlagRow {
            id: None,
            conversation_id: ConversationId::from(conversation_id.as_str()),
            from_seq: from,
            to_seq: to,
            label: label.clone(),
            tags: tags_vec,
            flagged_by: "controller".into(),
            flagged_at: chrono::Utc::now().timestamp(),
            notes,
        })
        .map_err(|e| anyhow::anyhow!("insert: {e}"))?;
    println!("flagged: id={id} conversation={conversation_id} range={from}..{to} label={label}");
    Ok(())
}

/// `execlaw eval list [--label X]` — print every eval flag.
fn cmd_eval_list(label: Option<String>, db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    use execlaw_core::eval::EvalFlaggedStore;

    let db = open_db(&db_path, no_encrypt)?;
    let store = EvalFlaggedStore::new(&db);
    let rows = match label.as_deref() {
        Some(l) => store
            .list_by_label(l)
            .map_err(|e| anyhow::anyhow!("list: {e}"))?,
        None => store.list_all().map_err(|e| anyhow::anyhow!("list: {e}"))?,
    };
    if rows.is_empty() {
        println!("(no flags)");
        return Ok(());
    }
    for r in rows {
        println!(
            "id={:<4} conv={:<24} range={:>4}..{:<4} label={:<24} tags={:?} flagged_at={}",
            r.id.unwrap_or_default(),
            r.conversation_id.as_str(),
            r.from_seq,
            r.to_seq,
            r.label,
            r.tags,
            r.flagged_at,
        );
        if let Some(n) = r.notes {
            println!("       notes: {n}");
        }
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct EvalRedactionMap {
    replacements: Vec<EvalRedactionReplacement>,
}

#[derive(Debug, serde::Deserialize)]
struct EvalRedactionReplacement {
    source: String,
    replacement: String,
}

#[derive(Default)]
struct EvalFixtureRedactor {
    replacements: Vec<(String, String)>,
    synthetic: std::collections::BTreeMap<(String, String), String>,
    applied: usize,
}

impl EvalFixtureRedactor {
    fn new(map: EvalRedactionMap) -> anyhow::Result<Self> {
        if map.replacements.is_empty() {
            anyhow::bail!("redaction map must provide at least one reviewed replacement");
        }
        let mut seen_sources = std::collections::HashSet::new();
        let mut seen_replacements = std::collections::HashSet::new();
        let mut replacements = Vec::with_capacity(map.replacements.len());
        for entry in map.replacements {
            if entry.source.trim().len() < 3
                || !entry.replacement.starts_with("<SYNTHETIC_")
                || !entry.replacement.ends_with('>')
                || entry.source == entry.replacement
                || !seen_sources.insert(entry.source.clone())
                || !seen_replacements.insert(entry.replacement.clone())
            {
                anyhow::bail!(
                    "redaction entries require unique source values and unique <SYNTHETIC_...> replacements"
                );
            }
            replacements.push((entry.source, entry.replacement));
        }
        replacements.sort_by_key(|(source, _)| std::cmp::Reverse(source.len()));
        Ok(Self {
            replacements,
            ..Self::default()
        })
    }

    fn synthetic_id(&mut self, kind: &str, raw: &str) -> String {
        let key = (kind.to_owned(), raw.to_owned());
        if let Some(existing) = self.synthetic.get(&key) {
            return existing.clone();
        }
        let replacement = format!(
            "<SYNTHETIC_{}_{}>",
            kind.to_ascii_uppercase(),
            self.synthetic.len() + 1
        );
        self.synthetic.insert(key, replacement.clone());
        self.applied += 1;
        replacement
    }

    fn redact_text(&mut self, input: &str) -> String {
        let mut text = input.to_owned();
        for (source, replacement) in &self.replacements {
            let count = text.matches(source).count();
            if count > 0 {
                text = text.replace(source, replacement);
                self.applied += count;
            }
        }
        for (kind, pattern) in sensitive_text_patterns() {
            text = pattern
                .replace_all(&text, |captures: &regex::Captures<'_>| {
                    let matched = captures
                        .get(0)
                        .map(|value| value.as_str())
                        .unwrap_or_default();
                    if *kind == "PHONE" && matched.chars().filter(char::is_ascii_digit).count() < 10
                    {
                        return matched.to_owned();
                    }
                    self.synthetic_id(kind, matched)
                })
                .into_owned();
        }
        text
    }

    fn redact_value(&mut self, value: &mut serde_json::Value) {
        use serde_json::Value;
        match value {
            Value::Object(fields) => {
                for (name, nested) in fields.iter_mut() {
                    let lower = name.to_ascii_lowercase();
                    if [
                        "password",
                        "secret",
                        "api_key",
                        "access_token",
                        "refresh_token",
                        "authorization",
                        "cookie",
                        "credential",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle))
                    {
                        *nested = Value::String("<REDACTED_SECRET>".into());
                        self.applied += 1;
                    } else if [
                        "principal_id",
                        "user_id",
                        "username",
                        "display_name",
                        "email",
                        "phone",
                        "native_id",
                        "recipient",
                        "conversation_id",
                        "assertion_id",
                        "evidence_id",
                        "extraction_run_id",
                        "reviewer_id",
                        "scope",
                    ]
                    .contains(&lower.as_str())
                    {
                        let raw = nested.as_str().unwrap_or("unknown").to_owned();
                        *nested = Value::String(self.synthetic_id("PERSON", &raw));
                    } else {
                        self.redact_value(nested);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.redact_value(item);
                }
            }
            Value::String(text) => *text = self.redact_text(text),
            _ => {}
        }
    }

    fn synthetic_ids(&self) -> Vec<String> {
        let mut ids = self.synthetic.values().cloned().collect::<Vec<_>>();
        ids.extend(
            self.replacements
                .iter()
                .map(|(_, replacement)| replacement.clone()),
        );
        ids.sort();
        ids.dedup();
        ids
    }
}

fn sensitive_text_patterns() -> &'static [(&'static str, regex::Regex)] {
    static PATTERNS: std::sync::OnceLock<Vec<(&'static str, regex::Regex)>> =
        std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            ("EMAIL", r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b"),
            (
                "JWT",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            ),
            ("SECRET", r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]+=*"),
            ("SECRET", r"\bsk-[A-Za-z0-9_-]{12,}\b"),
            ("PHONE", r"\+?[0-9][0-9(). -]{7,}[0-9]"),
            ("IP", r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b"),
        ]
        .into_iter()
        .map(|(kind, pattern)| {
            (
                kind,
                regex::Regex::new(pattern).expect("static redaction regex is valid"),
            )
        })
        .collect()
    })
}

fn cmd_memory_export_assertion(
    assertion_id: String,
    output_path: PathBuf,
    redaction_map_path: PathBuf,
    consent: bool,
    db_path: PathBuf,
    no_encrypt: bool,
) -> anyhow::Result<()> {
    use execlaw_core::events::{EventLog, KeyRing};
    use execlaw_core::{ConversationId, EventSeq};
    use sha2::{Digest, Sha256};
    use std::io::Write;

    if !consent {
        anyhow::bail!(
            "memory export requires --consent after reviewing the source and local redaction map"
        );
    }
    if output_path.exists() {
        anyhow::bail!(
            "memory export destination already exists: {}",
            output_path.display()
        );
    }
    let map_bytes = std::fs::read(&redaction_map_path)
        .map_err(|error| anyhow::anyhow!("read local redaction map: {error}"))?;
    let map: EvalRedactionMap = serde_json::from_slice(&map_bytes)
        .map_err(|error| anyhow::anyhow!("parse local redaction map: {error}"))?;
    let db = open_db(&db_path, no_encrypt)?;
    let store = execlaw_core::memory_assertions::MemoryAssertionStore::new(&db);
    let assertion = store
        .get(&assertion_id)?
        .ok_or_else(|| anyhow::anyhow!("memory assertion '{assertion_id}' was not found"))?;
    let evidence = store.evidence_for(&assertion_id, 501)?;
    if evidence.is_empty() {
        anyhow::bail!("memory assertion has no source evidence to export");
    }
    if evidence.len() > 500 {
        anyhow::bail!(
            "memory assertion has more than 500 evidence references; narrow the export first"
        );
    }
    let event_hmac_key = execlaw_vault::keyring_key::load_or_create_event_hmac_key()
        .map_err(|error| anyhow::anyhow!("event HMAC key: {error}"))?;
    let event_log = EventLog::new(&db).with_key_ring(KeyRing::single(0, event_hmac_key.to_vec()));
    let mut events_by_conversation = std::collections::HashMap::new();
    let mut exported_evidence = Vec::with_capacity(evidence.len());
    for source in evidence {
        if !events_by_conversation.contains_key(&source.conversation_id) {
            let events = event_log.replay_since(
                &ConversationId::from(source.conversation_id.clone()),
                EventSeq(0),
            )?;
            events_by_conversation.insert(source.conversation_id.clone(), events);
        }
        let source_event = events_by_conversation
            .get(&source.conversation_id)
            .and_then(|events| events.iter().find(|event| event.seq.0 == source.event_seq))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "source event {} is unavailable for evidence {}",
                    source.event_seq,
                    source.evidence_id
                )
            })?;
        let payload: serde_json::Value = source_event.decode_payload()?;
        let quote = execlaw_core::memory_assertions::evidence_quote(&payload, &source.payload_path)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "evidence payload path no longer resolves: {}",
                    source.payload_path
                )
            })?;
        let quote_hash = hex::encode(Sha256::digest(quote.as_bytes()));
        if quote_hash != source.quote_hash.to_ascii_lowercase() {
            anyhow::bail!(
                "source quote hash mismatch for evidence {}",
                source.evidence_id
            );
        }
        exported_evidence.push(serde_json::json!({
            "evidence_id": source.evidence_id,
            "conversation_id": source.conversation_id,
            "event_seq": source.event_seq,
            "payload_path": source.payload_path,
            "quote_hash": source.quote_hash,
            "evidence_kind": source.evidence_kind,
            "event_kind": source_event.kind.as_str(),
            "source_event_sha256": execlaw_core::harness::HarnessStore::fingerprint(source_event)?,
            "source_quote": quote,
        }));
    }

    let replacement_id_hash = execlaw_core::harness::HarnessStore::fingerprint(
        &map.replacements
            .iter()
            .map(|entry| &entry.replacement)
            .collect::<Vec<_>>(),
    )?;
    let mut redactor = EvalFixtureRedactor::new(map)?;
    let assertion_sha256 = execlaw_core::harness::HarnessStore::fingerprint(&assertion)?;
    let mut assertion_value = serde_json::to_value(&assertion)?;
    if store.is_retracted(&assertion_id)? {
        assertion_value["status"] = serde_json::Value::String("retracted".into());
    }
    let mut export = serde_json::json!({
        "schema_version": 1,
        "exported_at": chrono::Utc::now().timestamp(),
        "source_assertion_sha256": assertion_sha256,
        "assertion": assertion_value,
        "evidence": exported_evidence,
        "redaction": {
            "policy_version": "memory-redaction-v1",
            "replacement_ids_sha256": replacement_id_hash,
        },
        "privacy": "local_only",
    });
    redactor.redact_value(&mut export);
    let replacements_applied = redactor.applied;
    let synthetic_ids = redactor.synthetic_ids();
    export["redaction"]["replacements_applied"] = serde_json::json!(replacements_applied);
    export["redaction"]["synthetic_ids"] = serde_json::json!(synthetic_ids);
    let encoded = serde_json::to_vec_pretty(&export)?;
    if encoded.len() > 8 * 1024 * 1024 {
        anyhow::bail!("redacted memory export exceeds the 8 MiB limit");
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| anyhow::anyhow!("create memory export: {error}"))?;
    output.write_all(&encoded)?;
    println!(
        "memory assertion exported locally: evidence={} redactions={} path={}",
        export["evidence"].as_array().map_or(0, Vec::len),
        replacements_applied,
        output_path.display()
    );
    Ok(())
}

struct EvalExportOptions {
    output_path: PathBuf,
    redaction_map_path: PathBuf,
    consent: bool,
    incident_ref: Option<String>,
    release_ref: Option<String>,
    tool_catalog_snapshots_path: Option<PathBuf>,
    synthetic_media_path: Option<PathBuf>,
    policy_cases_path: Option<PathBuf>,
    db_path: PathBuf,
    no_encrypt: bool,
}

fn cmd_eval_export_flagged(id: i64, options: EvalExportOptions) -> anyhow::Result<()> {
    let EvalExportOptions {
        output_path,
        redaction_map_path,
        consent,
        incident_ref,
        release_ref,
        tool_catalog_snapshots_path,
        synthetic_media_path,
        policy_cases_path,
        db_path,
        no_encrypt,
    } = options;
    use execlaw_core::eval::{
        EvalFlaggedStore, ExpectedStateTransition, MockToolResponse, PolicyEvaluationFixture,
        RegressionFixture, RegressionFixtureEvent, RegressionFixtureProvenance,
        RegressionFixtureRedaction, SyntheticMediaFixture, ToolCatalogFixture,
        validate_regression_fixture,
    };
    use execlaw_core::events::{EventLog, KeyRing};
    use execlaw_core::ids::EventSeq;

    if !consent {
        anyhow::bail!(
            "fixture export contains selected conversation evidence; re-run with --consent after reviewing the redaction map"
        );
    }
    if output_path.exists() {
        anyhow::bail!(
            "fixture destination already exists: {}",
            output_path.display()
        );
    }
    let map_bytes = std::fs::read(&redaction_map_path)
        .map_err(|error| anyhow::anyhow!("read local redaction map: {error}"))?;
    let map: EvalRedactionMap = serde_json::from_slice(&map_bytes)
        .map_err(|error| anyhow::anyhow!("parse local redaction map: {error}"))?;
    let map_identity = map
        .replacements
        .iter()
        .map(|entry| &entry.replacement)
        .collect::<Vec<_>>();
    let redaction_map_sha256 = execlaw_core::harness::HarnessStore::fingerprint(&map_identity)?;
    let mut redactor = EvalFixtureRedactor::new(map)?;
    let synthetic_media: Vec<SyntheticMediaFixture> = match synthetic_media_path {
        Some(path) => {
            let bytes = std::fs::read(&path)
                .map_err(|error| anyhow::anyhow!("read synthetic media replacements: {error}"))?;
            serde_json::from_slice(&bytes)
                .map_err(|error| anyhow::anyhow!("parse synthetic media replacements: {error}"))?
        }
        None => Vec::new(),
    };
    let mut policy_cases: Vec<PolicyEvaluationFixture> = match policy_cases_path {
        Some(path) => {
            let bytes = std::fs::read(&path)
                .map_err(|error| anyhow::anyhow!("read policy evaluation cases: {error}"))?;
            serde_json::from_slice(&bytes)
                .map_err(|error| anyhow::anyhow!("parse policy evaluation cases: {error}"))?
        }
        None => Vec::new(),
    };
    let catalog_sidecar: Vec<ToolCatalogFixture> = match tool_catalog_snapshots_path {
        Some(path) => {
            let bytes = std::fs::read(&path)
                .map_err(|error| anyhow::anyhow!("read tool catalog snapshots: {error}"))?;
            if bytes.len() > 8 * 1024 * 1024 {
                anyhow::bail!("tool catalog snapshots exceed the 8 MiB sidecar limit");
            }
            serde_json::from_slice(&bytes)
                .map_err(|error| anyhow::anyhow!("parse tool catalog snapshots: {error}"))?
        }
        None => Vec::new(),
    };

    let db = open_db(&db_path, no_encrypt)?;
    let flag = EvalFlaggedStore::new(&db)
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("eval flag {id} was not found"))?;
    let hmac_key = execlaw_vault::keyring_key::load_or_create_event_hmac_key()
        .map_err(|error| anyhow::anyhow!("event HMAC key: {error}"))?;
    let conversation_id = flag.conversation_id.clone();
    let source_events = EventLog::new(&db)
        .with_key_ring(KeyRing::single(0, hmac_key.to_vec()))
        .replay_since(&conversation_id, EventSeq(flag.from_seq.saturating_sub(1)))?
        .into_iter()
        .filter(|event| event.seq.0 <= flag.to_seq)
        .collect::<Vec<_>>();
    if source_events.is_empty() {
        anyhow::bail!("flagged range contains no events");
    }
    if source_events.len() > 4096 {
        anyhow::bail!(
            "flagged range contains {} events; split it into ranges of at most 4096",
            source_events.len()
        );
    }

    let source_fingerprint_sha256 =
        execlaw_core::harness::HarnessStore::fingerprint(&source_events)?;
    let source_conversation_sha256 =
        execlaw_core::harness::HarnessStore::fingerprint(&conversation_id)?;
    let mut fixture_events = Vec::with_capacity(source_events.len());
    let mut expected_transitions = Vec::with_capacity(source_events.len());
    let mut mock_tool_responses = Vec::new();
    let mut current_turn_seq = flag.from_seq;
    for event in source_events {
        let mut payload: serde_json::Value =
            rmp_serde::from_slice(&event.payload).map_err(|error| {
                anyhow::anyhow!("decode event {} for redaction: {error}", event.seq.0)
            })?;
        redactor.redact_value(&mut payload);
        if event.kind.as_str() == "user_msg" {
            current_turn_seq = event.seq.0;
        }
        let actor = event.actor.map(|actor| match actor.as_str() {
            "system" => "system".to_owned(),
            "agent" => "agent".to_owned(),
            other => redactor.synthetic_id("ACTOR", other),
        });
        if event.kind.as_str() == "tool_result" {
            let ordinal = payload
                .get("ordinal")
                .and_then(serde_json::Value::as_u64)
                .and_then(|ordinal| u32::try_from(ordinal).ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("tool_result event {} has no ordinal", event.seq.0)
                })?;
            mock_tool_responses.push(MockToolResponse {
                turn_seq: current_turn_seq,
                ordinal,
                payload: payload.clone(),
            });
        }
        let kind = event.kind.as_str().to_owned();
        expected_transitions.push(ExpectedStateTransition {
            seq: event.seq.0,
            transition: kind.clone(),
        });
        fixture_events.push(RegressionFixtureEvent {
            seq: event.seq.0,
            kind,
            actor,
            committed_at: event.committed_at,
            payload,
        });
    }

    for case in &mut policy_cases {
        case.case_id = redactor.synthetic_id("POLICY_CASE", &case.case_id);
    }

    let synthetic_ids = redactor.synthetic_ids();
    if synthetic_media
        .iter()
        .any(|media| !synthetic_ids.contains(&media.attachment_id))
    {
        anyhow::bail!("synthetic media IDs must be replacement IDs from the local redaction map");
    }
    let has_attachments = fixture_events.iter().any(|event| {
        event.kind == "user_msg"
            && event
                .payload
                .get("attachment_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| !ids.is_empty())
    });
    if has_attachments && synthetic_media.is_empty() {
        anyhow::bail!(
            "flagged range has attachments; provide --synthetic-media with synthetic replacements, source bytes are never exported"
        );
    }

    let mut tool_catalogs = Vec::new();
    for user_event in fixture_events
        .iter()
        .filter(|event| event.kind == "user_msg")
    {
        if let Some(snapshot) =
            export_tool_catalog_for_turn(&db, &conversation_id, user_event.seq, &mut redactor)?
        {
            tool_catalogs.push(snapshot);
        }
    }
    let user_event_seqs = fixture_events
        .iter()
        .filter(|event| event.kind == "user_msg")
        .map(|event| event.seq)
        .collect::<std::collections::HashSet<_>>();
    merge_tool_catalog_sidecars(
        &db,
        &conversation_id,
        &user_event_seqs,
        &mut tool_catalogs,
        catalog_sidecar,
        &mut redactor,
    )?;

    let fixture = RegressionFixture {
        schema_version: 1,
        effects_enabled: false,
        provenance: RegressionFixtureProvenance {
            flagged_range_id: id,
            source_conversation_sha256,
            source_fingerprint_sha256,
            label: redactor.redact_text(&flag.label),
            tags: flag
                .tags
                .iter()
                .map(|tag| redactor.redact_text(tag))
                .collect(),
            from_seq: flag.from_seq,
            to_seq: flag.to_seq,
            flagged_at: flag.flagged_at,
            exported_at: chrono::Utc::now().timestamp(),
            incident_ref,
            release_ref,
        },
        redaction: RegressionFixtureRedaction {
            policy_version: "redaction-v1".into(),
            redaction_map_sha256,
            replacements_applied: redactor.applied,
            synthetic_ids,
        },
        events: fixture_events,
        expected_transitions,
        mock_tool_responses,
        raw_stream_fixtures: Vec::new(),
        synthetic_media,
        policy_cases,
        tool_catalogs,
    };
    validate_regression_fixture(&fixture)
        .map_err(|error| anyhow::anyhow!("fixture rejected by offline validator: {error}"))?;
    let encoded = serde_json::to_vec_pretty(&fixture)?;
    if encoded.len() > 8 * 1024 * 1024 {
        anyhow::bail!(
            "redacted fixture exceeds the 8 MiB export limit; select a narrower flag range"
        );
    }
    use std::io::Write;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| anyhow::anyhow!("create fixture output: {error}"))?;
    output.write_all(&encoded)?;
    println!(
        "fixture exported: flag_id={id} events={} redactions={} effects_enabled=false incident={} release={} path={}",
        fixture.events.len(),
        fixture.redaction.replacements_applied,
        fixture
            .provenance
            .incident_ref
            .as_deref()
            .unwrap_or("unlinked"),
        fixture
            .provenance
            .release_ref
            .as_deref()
            .unwrap_or("unlinked"),
        output_path.display()
    );
    Ok(())
}

fn export_tool_catalog_for_turn(
    db: &execlaw_core::Database,
    conversation_id: &execlaw_core::ConversationId,
    turn_seq: i64,
    redactor: &mut EvalFixtureRedactor,
) -> anyhow::Result<Option<execlaw_core::eval::ToolCatalogFixture>> {
    use execlaw_core::eval::ToolCatalogFixture;
    use execlaw_core::ids::EventSeq;
    use execlaw_core::runs::RunStore;

    let runs = RunStore::new(db);
    let Some(run) = runs.for_input_event(conversation_id, EventSeq(turn_seq))? else {
        return Ok(None);
    };
    let Some(manifest) = runs.input_manifest(&run.run_id)? else {
        return Ok(None);
    };
    let Some(snapshot_json) = manifest.tool_catalog_snapshot_json else {
        return Ok(None);
    };
    let mut snapshot: serde_json::Value =
        serde_json::from_str(&snapshot_json).map_err(|error| {
            anyhow::anyhow!("decode persisted tool catalog for fixture export: {error}")
        })?;
    let tools = snapshot
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("persisted tool catalog omitted tools"))?;
    let discoverable_tools = snapshot
        .get("discoverable_tools")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("persisted tool catalog omitted discoverable tools"))?;
    let mut pinned = discoverable_tools.clone();
    pinned.extend(tools.iter().cloned());
    if execlaw_core::tool::tool_schema_hash(&serde_json::Value::Array(pinned))
        != manifest.tool_catalog_hash
    {
        anyhow::bail!("persisted tool catalog does not match its immutable run manifest");
    }
    redactor.redact_value(&mut snapshot);
    let tools = snapshot["tools"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("redacted tool catalog omitted tools"))?;
    let discoverable_tools = snapshot["discoverable_tools"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("redacted tool catalog omitted discoverable tools"))?;
    Ok(Some(ToolCatalogFixture {
        turn_seq,
        source_catalog_hash: manifest.tool_catalog_hash,
        snapshot_sha256: execlaw_core::harness::HarnessStore::fingerprint(&snapshot)?,
        tools,
        discoverable_tools,
    }))
}

fn merge_tool_catalog_sidecars(
    db: &execlaw_core::Database,
    conversation_id: &execlaw_core::ConversationId,
    user_event_seqs: &std::collections::HashSet<i64>,
    catalogs: &mut Vec<execlaw_core::eval::ToolCatalogFixture>,
    sidecar: Vec<execlaw_core::eval::ToolCatalogFixture>,
    redactor: &mut EvalFixtureRedactor,
) -> anyhow::Result<()> {
    use execlaw_core::runs::RunStore;

    let runs = RunStore::new(db);
    for mut catalog in sidecar {
        if !user_event_seqs.contains(&catalog.turn_seq)
            || catalogs
                .iter()
                .any(|existing| existing.turn_seq == catalog.turn_seq)
        {
            anyhow::bail!("tool catalog sidecar has an unknown or duplicate turn sequence");
        }
        let mut snapshot = serde_json::json!({
            "tools":catalog.tools,
            "discoverable_tools":catalog.discoverable_tools,
        });
        if execlaw_core::harness::HarnessStore::fingerprint(&snapshot)? != catalog.snapshot_sha256 {
            anyhow::bail!("tool catalog sidecar snapshot hash does not match its declarations");
        }
        if let Some(run) =
            runs.for_input_event(conversation_id, execlaw_core::EventSeq(catalog.turn_seq))?
        {
            if let Some(manifest) = runs.input_manifest(&run.run_id)? {
                if manifest.tool_catalog_hash != catalog.source_catalog_hash {
                    anyhow::bail!("tool catalog sidecar does not match the run's catalog hash");
                }
                if manifest.tool_catalog_snapshot_json.is_some() {
                    anyhow::bail!("tool catalog sidecar duplicates an available durable snapshot");
                }
            }
        }
        redactor.redact_value(&mut snapshot);
        catalog.tools = snapshot["tools"]
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("redacted tool catalog sidecar omitted tools"))?;
        catalog.discoverable_tools = snapshot["discoverable_tools"]
            .as_array()
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("redacted tool catalog sidecar omitted discoverable tools")
            })?;
        catalog.snapshot_sha256 = execlaw_core::harness::HarnessStore::fingerprint(&snapshot)?;
        catalogs.push(catalog);
    }
    Ok(())
}

// ----- Phase 7 hardening commands -------------------------------------

fn cmd_backfill_events(db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    use execlaw_core::events::{EventLog, KeyRing};

    let db = open_db(&db_path, no_encrypt)?;
    // Use the operator's keyring-backed master key so back-fill
    // produces tags that match what `serve` would have produced
    // had a key been attached at append time.
    let key = execlaw_vault::keyring_key::load_or_create_event_hmac_key()
        .map_err(|e| anyhow::anyhow!("event HMAC key: {e}"))?;
    let log = EventLog::new(&db).with_key_ring(KeyRing::single(0, key.to_vec()));
    let report = log
        .backfill_null_tags()
        .map_err(|e| anyhow::anyhow!("back-fill: {e}"))?;
    println!(
        "backfill: signed={} skipped={} null_remaining={}",
        report.signed, report.skipped, report.null_remaining,
    );
    Ok(())
}

/// Recovery: re-sign every state_events row under the current HMAC
/// key, overwriting the existing tag. Use only when the original
/// signing key was lost (e.g. OS keyring lost the entry) and the
/// operator accepts that the existing log is now signed under a new
/// key — the tamper-evidence guarantee for already-stored rows is
/// gone.
fn cmd_resign_events(db_path: PathBuf, no_encrypt: bool, confirmed: bool) -> anyhow::Result<()> {
    use execlaw_core::events::{EventLog, KeyRing};

    if !confirmed {
        anyhow::bail!(
            "resign-events destroys the tamper-evidence guarantee for existing rows. \
             Re-run with --i-understand-history-will-be-resigned to proceed."
        );
    }

    let db = open_db(&db_path, no_encrypt)?;
    let key = execlaw_vault::keyring_key::load_or_create_event_hmac_key()
        .map_err(|e| anyhow::anyhow!("event HMAC key: {e}"))?;
    let log = EventLog::new(&db).with_key_ring(KeyRing::single(0, key.to_vec()));
    let report = log
        .resign_all_with_current_key()
        .map_err(|e| anyhow::anyhow!("resign: {e}"))?;
    println!("resign: signed={} (overwrote existing tags)", report.signed);
    Ok(())
}

fn cmd_backup(to: PathBuf, db_path: PathBuf, no_encrypt: bool) -> anyhow::Result<()> {
    if !db_path.exists() {
        anyhow::bail!("source db not found: {}", db_path.display());
    }
    let db = open_db(&db_path, no_encrypt)?;
    write_backup_file(&db, &to)?;

    verify_database_snapshot(&to, no_encrypt)?;

    println!(
        "backup: {} -> {} ({} bytes)",
        db_path.display(),
        to.display(),
        std::fs::metadata(&to).map(|m| m.len()).unwrap_or_default()
    );
    Ok(())
}

fn write_backup_file(db: &execlaw_core::Database, to: &Path) -> anyhow::Result<()> {
    if let Some(parent) = to.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            anyhow::bail!(
                "backup parent directory does not exist: {}",
                parent.display()
            );
        }
    }
    if to.exists() {
        anyhow::bail!("backup target already exists: {}", to.display());
    }
    let to_str = to
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 path: {}", to.display()))?;
    db.with_conn(|connection| {
        // VACUUM INTO requires a quoted path rather than a bind parameter.
        // The path is operator-supplied and SQL-quote-escaped here.
        // nosemgrep: rust-rusqlite-format-arg
        connection.execute_batch(&format!("VACUUM INTO '{}'", to_str.replace('\'', "''")))?;
        Ok(())
    })
    .map_err(|error| anyhow::anyhow!("VACUUM INTO: {error}"))
}

fn cmd_rotate_keys(backup_path: PathBuf, db_path: PathBuf, confirmed: bool) -> anyhow::Result<()> {
    if !confirmed {
        anyhow::bail!(
            "key rotation changes the SQLCipher key; event history remains signed with its stable HMAC key. \
             pass --i-understand-database-key-will-change to proceed"
        );
    }
    #[cfg(not(feature = "sqlcipher"))]
    {
        let _ = (backup_path, db_path);
        anyhow::bail!("key rotation requires an execlaw binary built with --features sqlcipher");
    }
    #[cfg(feature = "sqlcipher")]
    {
        use rand::RngCore;

        if db_path == backup_path {
            anyhow::bail!("backup path must differ from the live database path");
        }
        if backup_path.exists() || backup_path.with_extension("rotation-pre.db").exists() {
            anyhow::bail!("rotation backup path or temporary old-key snapshot already exists");
        }
        let old_key = execlaw_vault::load_or_create_master_key()
            .map_err(|error| anyhow::anyhow!("load current master key: {error}"))?;
        let event_hmac_key = execlaw_vault::keyring_key::load_or_create_event_hmac_key()
            .map_err(|error| anyhow::anyhow!("load event HMAC key: {error}"))?;
        // Keep an old-key snapshot until the new-key snapshot has been
        // verified and the durable key file has switched successfully.
        let old_backup = backup_path.with_extension("rotation-pre.db");
        cmd_backup(old_backup.clone(), db_path.clone(), false)?;
        let (db, _) = open_db_with_config(&db_path, false)?;
        let mut new_key = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut new_key);

        db.rekey_sqlcipher(&new_key)
            .map_err(|error| anyhow::anyhow!("re-encrypt database: {error}"))?;

        let snapshot_result = write_backup_file(&db, &backup_path).and_then(|()| {
            verify_database_snapshot_with_keys(&backup_path, &new_key, &event_hmac_key)
        });
        if let Err(error) = snapshot_result {
            rollback_master_key_rotation(&db, &old_key)?;
            let _ = std::fs::remove_file(&backup_path);
            return Err(anyhow::anyhow!("verify new-key recovery snapshot: {error}"));
        }

        let key_path = execlaw_vault::keyring_key::default_passphrase_file_path();
        if let Err(error) =
            execlaw_vault::keyring_key::persist_rotated_master_key(&key_path, &new_key)
        {
            rollback_master_key_rotation(&db, &old_key)?;
            let _ = std::fs::remove_file(&backup_path);
            return Err(anyhow::anyhow!("persist rotated key: {error}"));
        }
        let _ = std::fs::remove_file(&old_backup);
        println!(
            "database key rotation complete; verified recovery snapshot: {}; event HMAC chain preserved",
            backup_path.display(),
        );
        Ok(())
    }
}

#[cfg(feature = "sqlcipher")]
fn rollback_master_key_rotation(
    db: &execlaw_core::Database,
    old_key: &[u8; 32],
) -> anyhow::Result<()> {
    db.rekey_sqlcipher(old_key).map_err(|error| {
        anyhow::anyhow!("rotation rollback could not restore old database key: {error}")
    })?;
    Ok(())
}

fn cmd_restore(
    from: PathBuf,
    db_path: PathBuf,
    force: bool,
    no_encrypt: bool,
) -> anyhow::Result<()> {
    if !from.exists() {
        anyhow::bail!("snapshot file not found: {}", from.display());
    }

    // Validate the snapshot first: it must open with the operator's
    // master key AND carry the schema_version table. Otherwise
    // restoring would silently swap in a useless DB.
    verify_database_snapshot(&from, no_encrypt)?;

    if db_path.exists() && !force {
        let size = std::fs::metadata(&db_path)
            .map(|m| m.len())
            .unwrap_or_default();
        if size > 0 {
            anyhow::bail!(
                "target {} is non-empty ({} bytes); pass --force to overwrite",
                db_path.display(),
                size,
            );
        }
    }

    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    // Atomic-ish: write to a sibling tempfile, then rename. Rename
    // on the same filesystem is atomic on every supported OS.
    let tmp = db_path.with_extension("restore.tmp");
    if tmp.exists() {
        std::fs::remove_file(&tmp)?;
    }
    std::fs::copy(&from, &tmp)?;
    let existing_bytes = db_path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    let preserved_tombstones = if existing_bytes > 0 {
        let current_db = open_db(&db_path, no_encrypt)?;
        let restored_db = open_db(&tmp, no_encrypt)?;
        let count = privacy_restore::reapply_from(&current_db, &restored_db)?;
        drop(restored_db);
        drop(current_db);
        count
    } else {
        0
    };
    verify_database_snapshot(&tmp, no_encrypt)?;
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }
    std::fs::rename(&tmp, &db_path)?;

    println!(
        "restore: {} -> {} ({} bytes, {} privacy tombstones reapplied)",
        from.display(),
        db_path.display(),
        std::fs::metadata(&db_path)
            .map(|m| m.len())
            .unwrap_or_default(),
        preserved_tombstones,
    );
    Ok(())
}

fn verify_database_snapshot(path: &Path, no_encrypt: bool) -> anyhow::Result<()> {
    let key = if no_encrypt {
        None
    } else {
        Some(
            execlaw_vault::load_or_create_master_key()
                .map_err(|error| anyhow::anyhow!("master key: {error}"))?,
        )
    };
    let hmac_key = if no_encrypt {
        None
    } else {
        Some(
            execlaw_vault::keyring_key::load_or_create_event_hmac_key()
                .map_err(|error| anyhow::anyhow!("event HMAC key: {error}"))?,
        )
    };
    verify_database_snapshot_inner(path, key.as_ref(), hmac_key.as_ref())
}

#[cfg(feature = "sqlcipher")]
fn verify_database_snapshot_with_keys(
    path: &Path,
    encryption_key: &[u8; 32],
    hmac_key: &[u8; 32],
) -> anyhow::Result<()> {
    verify_database_snapshot_inner(path, Some(encryption_key), Some(hmac_key))
}

fn verify_database_snapshot_inner(
    path: &Path,
    encryption_key: Option<&[u8; 32]>,
    hmac_key: Option<&[u8; 32]>,
) -> anyhow::Result<()> {
    let snapshot = execlaw_core::Database::open(&execlaw_core::DbConfig {
        path: path.to_path_buf(),
        key: encryption_key.map(|raw| execlaw_core::db::SqlCipherKey::RawBytes(raw.to_vec())),
    })?;
    let (integrity, has_version): (String, bool) = snapshot.with_conn(|connection| {
        let integrity = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok((integrity, count > 0))
    })?;
    if integrity != "ok" {
        anyhow::bail!(
            "snapshot at {} failed SQLite integrity check",
            path.display()
        );
    }
    if !has_version {
        anyhow::bail!(
            "snapshot at {} doesn't look like an execlaw DB (missing schema_version table)",
            path.display()
        );
    }
    if let Some(key) = hmac_key {
        use execlaw_core::events::{EventLog, KeyRing};
        use execlaw_core::ids::{ConversationId, EventSeq};

        let conversations = snapshot.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT conversation_id FROM state_conversations ORDER BY conversation_id",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(Into::into)
        })?;
        let log = EventLog::new(&snapshot).with_key_ring(KeyRing::single(0, key.to_vec()));
        for conversation_id in conversations {
            log.replay_since(&ConversationId::from(conversation_id.as_str()), EventSeq(0))
                .map_err(|error| anyhow::anyhow!("snapshot event-chain verification: {error}"))?;
        }
    }
    Ok(())
}

/// Build a WebAuthn relying-party from environment variables. Returns
/// `None` (so login falls back to password-only) on any error so an
/// operator who hasn't yet configured WebAuthn isn't locked out.
///
/// `EXECLAW_WEBAUTHN_RP_ID` is the effective domain (hostname only —
/// no scheme, no port). Defaults to `"localhost"`.
/// `EXECLAW_WEBAUTHN_ORIGIN` is the full origin used to build the URL
/// passed to webauthn-rs. Defaults to `http://<bind_addr>` so a
/// fresh-from-clone install Just Works for local-dev.
fn build_webauthn_from_env(
    bind_addr: &std::net::SocketAddr,
) -> Option<execlaw_server::webauthn::WebauthnSvc> {
    let rp_id = std::env::var("EXECLAW_WEBAUTHN_RP_ID").unwrap_or_else(|_| "localhost".to_owned());
    let origin =
        std::env::var("EXECLAW_WEBAUTHN_ORIGIN").unwrap_or_else(|_| format!("http://{bind_addr}"));
    match execlaw_server::webauthn::WebauthnSvc::new(&rp_id, &origin, "execlaw") {
        Ok(svc) => Some(svc),
        Err(e) => {
            tracing::warn!(
                rp_id,
                origin,
                error = %e,
                "webauthn relying-party build failed; falling back to password-only login"
            );
            None
        }
    }
}

/// Parse `12..48` (inclusive on both ends).
fn parse_range(s: &str) -> anyhow::Result<(i64, i64)> {
    let mut parts = s.splitn(2, "..");
    let from: i64 = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("range '{s}' missing 'from'"))?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad from in '{s}': {e}"))?;
    let to: i64 = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("range '{s}' missing 'to' (use a..b)"))?
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("bad to in '{s}': {e}"))?;
    Ok((from, to))
}

/// Pick the bind address the listener will use. Precedence:
///
///   1. The `--bind` CLI flag, if passed (one-off overrides for dev).
///   2. `config_general.bind_address` from the DB, if a row exists
///      (the SPA's Settings → General writes here).
///   3. `127.0.0.1:3031` — the install-time hardcoded default.
///
/// Returns the resolved value plus a short source label suitable for
/// the boot log, so an operator chasing "why am I bound to X" has a
/// breadcrumb.
fn resolve_bind(cli: Option<String>, db: Option<String>) -> (String, &'static str) {
    if let Some(s) = cli {
        return (s, "cli");
    }
    if let Some(s) = db.filter(|s| !s.trim().is_empty()) {
        return (s, "config_general");
    }
    ("127.0.0.1:3031".to_string(), "default")
}

const SHUTDOWN_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not register SIGTERM handler; waiting for Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn trigger_shutdown(
    service_shutdown: Option<tokio::sync::oneshot::Receiver<()>>,
    trigger: tokio::sync::oneshot::Sender<()>,
) {
    if let Some(service_shutdown) = service_shutdown {
        let _ = service_shutdown.await;
    } else {
        wait_for_shutdown_signal().await;
    }
    let _ = trigger.send(());
}

fn join_server_result(
    result: Result<std::io::Result<()>, tokio::task::JoinError>,
    phase: &str,
) -> anyhow::Result<()> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error.into()),
        Err(error) => Err(anyhow::anyhow!("server task failed {phase}: {error}")),
    }
}

async fn cmd_serve(
    bind: Option<String>,
    db_path: PathBuf,
    no_encrypt: bool,
    allow_unsigned_local_development: bool,
    service_shutdown_rx: Option<tokio::sync::oneshot::Receiver<()>>,
) -> anyhow::Result<()> {
    // Register process/SCM shutdown before opening the database or spawning
    // workers so a stop requested during startup is retained until Axum is
    // ready to perform the graceful drain.
    let (shutdown_trigger, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut shutdown_signal_task =
        tokio::spawn(trigger_shutdown(service_shutdown_rx, shutdown_trigger));
    let (db, db_config) = open_db_with_config(&db_path, no_encrypt)?;
    execlaw_core::MigrationRunner::new(&db).apply_all()?;
    // Advance completed checkpoints and empty cursors automatically. This
    // touches no model, approval, tool, or external-effect operation; those
    // require their owning executor to revalidate before resuming.
    let run_store = execlaw_core::runs::RunStore::new(&db);
    let recovered_transitions =
        run_store.recover_completed_transitions(chrono::Utc::now().timestamp(), 500, 2_000)?;
    for transition in &recovered_transitions {
        tracing::info!(
            run_id = %transition.run_id,
            action = transition.action,
            from_cursor = transition.from_cursor,
            to_cursor = transition.to_cursor,
            status = ?transition.resulting_status,
            "durable run recovered without redispatch"
        );
    }
    // Inventory the remaining transitions. Tool and outbox work still belongs
    // to its executor and must reconcile its sink before it can be resumed.
    let recovery_candidates = run_store.recovery_candidates(chrono::Utc::now().timestamp(), 500)?;
    if !recovery_candidates.is_empty() {
        tracing::warn!(
            recoverable_run_count = recovery_candidates.len(),
            "durable runs require executor recovery"
        );
        for candidate in &recovery_candidates {
            let (next_action, step_id, step_kind) = match &candidate.next_action {
                execlaw_core::runs::NextSafeAction::Claim(step) => (
                    "claim",
                    Some(step.step_id.as_str()),
                    Some(step.kind.as_str()),
                ),
                execlaw_core::runs::NextSafeAction::ReclaimExpired(step) => (
                    "reclaim_expired_lease",
                    Some(step.step_id.as_str()),
                    Some(step.kind.as_str()),
                ),
                execlaw_core::runs::NextSafeAction::WaitForLease(step) => (
                    "wait_for_lease",
                    Some(step.step_id.as_str()),
                    Some(step.kind.as_str()),
                ),
                execlaw_core::runs::NextSafeAction::WaitForApproval(step) => (
                    "wait_for_approval",
                    Some(step.step_id.as_str()),
                    Some(step.kind.as_str()),
                ),
                execlaw_core::runs::NextSafeAction::Wait(step) => (
                    "wait_for_step",
                    Some(step.step_id.as_str()),
                    Some(step.kind.as_str()),
                ),
                execlaw_core::runs::NextSafeAction::AdvanceCursor(step) => (
                    "advance_completed_checkpoint",
                    Some(step.step_id.as_str()),
                    Some(step.kind.as_str()),
                ),
                execlaw_core::runs::NextSafeAction::CompleteRun { .. } => {
                    ("complete_empty_cursor", None, None)
                }
                execlaw_core::runs::NextSafeAction::RunCompleted => {
                    ("already_completed", None, None)
                }
                execlaw_core::runs::NextSafeAction::RunFailed => ("failed", None, None),
                execlaw_core::runs::NextSafeAction::RunCancelled => ("cancelled", None, None),
            };
            tracing::warn!(
                run_id = %candidate.run.run_id,
                conversation_id = %candidate.run.conversation_id,
                status = ?candidate.run.status,
                cursor = candidate.run.cursor,
                next_action,
                step_id = step_id.unwrap_or(""),
                step_kind = step_kind.unwrap_or(""),
                "recoverable run discovered during startup"
            );
        }
    }
    let provenance_store =
        execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(db.clone());
    if allow_unsigned_local_development {
        let mut artifact_policy = provenance_store.policy()?;
        if !artifact_policy.allow_unsigned_local_development {
            artifact_policy.allow_unsigned_local_development = true;
            provenance_store.configure("Controller", "execlaw serve", &artifact_policy)?;
        }
    }

    // Resolve the data directory once at boot so downstream code
    // (bundled-plugins mirror, settings paths, etc.) doesn't have
    // to re-derive it. `db_path` always lives under the data dir
    // by construction (cli/main.rs::default_db_path returns
    // `<data_dir>/execlaw.db`); pull the parent and fall back to
    // `default_data_dir()` for operators who explicitly pointed
    // --db elsewhere.
    let data_dir = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_data_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // Mirror any plugin ZIPs that ship inside the .app's
    // Contents/Resources/plugins/ into <data_dir>/bundled-plugins/.
    // Idempotent + best-effort — see crates/server/src/bundled_plugins.rs.
    // Linux/Windows installs (no .app shell, no env override) are
    // a silent no-op; operators drop ZIPs into that directory by
    // hand and the SPA's "Bundled" section still lists them.
    execlaw_server::bundled_plugins::mirror_bundled_plugins_into_data_dir(&data_dir);

    // Bind address resolution (precedence: CLI flag > DB > default).
    // The DB-stored value comes from Settings → General; making it
    // authoritative here is what allows the SPA's "takes effect on
    // next restart" hint to be true.
    let db_bind = execlaw_core::general_settings::GeneralSettingsStore::new(&db)
        .get()
        .ok()
        .flatten()
        .map(|s| s.bind_address);
    let (bind, bind_source) = resolve_bind(bind, db_bind);
    tracing::info!(addr = %bind, source = bind_source, "resolved bind address");

    // 2026-04-28 — derive the JWT signing key from the vault's
    // master key. Pre-fix this was `JwtSigner::generate(...)` which
    // minted a fresh keypair on every boot, silently invalidating
    // every previously-issued access_token whenever cargo-watch
    // rebuilt. Now: same master across boots → same JWT signing
    // key → tokens survive the rebuild. Fall back to the random
    // generator only when the keyring isn't reachable AT ALL
    // (rare; we'd already be running with a degraded vault).
    let signer = match execlaw_vault::load_or_create_master_key() {
        Ok(master) => std::sync::Arc::new(execlaw_server::auth::JwtSigner::from_master_key(
            &master,
            "execlaw".into(),
        )),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not load vault master key; JWT signing key will be ephemeral. \
                 Operators will be signed out on every restart."
            );
            std::sync::Arc::new(execlaw_server::auth::JwtSigner::generate("execlaw".into()))
        }
    };
    // Phase-7 hardening: refresh tokens persist in SQLite so a
    // server restart no longer signs every operator out.
    let refresh_store = std::sync::Arc::new(execlaw_server::auth::RefreshStore::new(db.clone()));

    // EXECLAW_INFERENCE_URL lets operators point dev servers at a local
    // vLLM / Ollama / OpenArc without editing code. Production boots
    // will read the active Standard deployment from
    // `config_runner_deployments` once the registry API lands.
    let inference_base_url = std::env::var("EXECLAW_INFERENCE_URL").ok();

    let config = std::sync::Arc::new(execlaw_server::ServerConfig {
        bind_addr: bind.parse()?,
        log_dir: resolve_log_dir(),
        ..Default::default()
    });

    // Phase 12.E — bootstrap is the boot-time global URL; per-turn
    // resolution may override it via config_backends rows.
    let bootstrap_inference = inference_base_url.map(|url| {
        // Allow operators to provide a bearer token for bootstrapped
        // inference endpoints via env var. Support both
        // `EXECLAW_INFERENCE_API_KEY` (preferred) and the legacy
        // `EXECLAW_INFERENCE_KEY` name if present.
        let mut client = execlaw_inference_api::InferenceClient::new(url);
        let api_key = std::env::var("EXECLAW_INFERENCE_API_KEY")
            .or_else(|_| std::env::var("EXECLAW_INFERENCE_KEY"))
            .ok();
        if let Some(k) = api_key {
            if !k.trim().is_empty() {
                client = client.with_api_key(k);
            }
        }

        // Optional engine override: set `EXECLAW_INFERENCE_ENGINE=ollama`
        // to force the bootstrap client to use Ollama's native `/api/chat`
        // path instead of the OpenAI-compat `/v1/chat/completions`.
        if let Ok(e) = std::env::var("EXECLAW_INFERENCE_ENGINE") {
            if e.eq_ignore_ascii_case("ollama") {
                client = client.with_engine(execlaw_inference_api::InferenceEngine::Ollama);
            }
        }

        std::sync::Arc::new(client)
    });
    let inference = std::sync::Arc::new(
        execlaw_server::inference_resolver::InferenceResolver::new(bootstrap_inference),
    );

    // Keep event signing stable when SQLCipher's encryption key rotates.
    let hmac_key = Some(std::sync::Arc::new(
        execlaw_vault::keyring_key::load_or_create_event_hmac_key()
            .map_err(|error| anyhow::anyhow!("event HMAC key: {error}"))?
            .to_vec(),
    ));
    if let Some(key) = hmac_key.as_ref() {
        db.set_event_hmac_key((**key).clone())?;
    }

    // Stage root for installed plugins — defaults to
    // `<db_parent>/plugins/`. Each install lands under
    // `<stage_root>/<plugin_id>-<version>/`.
    let stage_root = db_path
        .parent()
        .map(|p| p.join("plugins"))
        .unwrap_or_else(|| PathBuf::from("./plugins"));
    if let Err(e) = std::fs::create_dir_all(&stage_root) {
        tracing::warn!(path = ?stage_root, error = %e, "failed to ensure plugin stage root");
    }
    let plugin_host = execlaw_plugin_host::PluginHost::new(
        db.clone(),
        execlaw_plugin_host::HookRegistry::new(),
        stage_root,
    );
    let _ = plugin_host.attach_attestation_verifier(std::sync::Arc::new(
        execlaw_core::artifact_provenance::CosignCliVerifier::new("cosign"),
    ));
    // Re-hydrate installed plugins from the DB so they survive restart.
    plugin_host
        .hydrate()
        .await
        .map_err(|e| anyhow::anyhow!("plugin hydrate: {e}"))?;

    // 2026-04-29 — register the core trait-based built-in tools
    // (read_memory, write_memory, list_memory, set_thread_name,
    // get_thread) into the host's HookRegistry and seed their
    // `config_tool_access` rows from each descriptor's
    // `default_allowed_classes`. Must run BEFORE sync_tool_access so
    // the access sync sees them in `registry.all_builtins()`.
    {
        let now = chrono::Utc::now().timestamp();
        match execlaw_plugin_host::register_core_builtins(plugin_host.registry(), &db, now) {
            Ok(landed) => tracing::info!(count = landed.len(), "core built-in tools registered"),
            Err(e) => {
                // Conflict here means an operator-installed plugin is
                // claiming a tool name that overlaps with a core
                // built-in — the plugin install should have rejected
                // that, but if it slipped through we can't proceed
                // safely with the overlap.
                return Err(anyhow::anyhow!("register_core_builtins failed: {e}"));
            }
        }
    }

    // 2026-05-04 — Phase 3 (signal sidecar): register the two
    // host-implemented Signal tools as builtins so they can reach
    // `ctx.transport`. The plugin manifest declares them with
    // `host_implemented = true` so the rhai tier doesn't try to
    // (Phase B removed: signal_tools host registration. The signal
    // plugin v0.4.0+ ships every tool in main.rhai — dispatch hits
    // the script tier through the standard plugin-tool path. No
    // host-side wiring needed here anymore.)

    // 2026-05-03 — Phase A: register the skill subsystem's tool
    // surface (skills.list/view/resource/search + admin-gated
    // create/update/promote/archive). Uses the same
    // `register_builtins` helper as the core tools so each skill
    // tool also gets a `config_tool_access` seed row from its
    // descriptor's `default_allowed_classes`. The store is shared
    // across all eight tools via Arc; it holds only a Database
    // handle so the clone is cheap.
    let skill_store = std::sync::Arc::new(execlaw_skills::SkillStore::new(db.clone()));
    // Load operator-managed markdown skills from the portable data
    // directory. The importer is idempotent, so deploys and restarts
    // do not create a new DB version for unchanged files.
    match execlaw_skills::import_filesystem_skills(
        &skill_store,
        &data_dir.join("skills"),
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(count) if count > 0 => tracing::info!(count, "filesystem skills imported"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "filesystem skill import failed"),
    }
    {
        let now = chrono::Utc::now().timestamp();
        let tools = execlaw_skills::skill_tools(skill_store.clone());
        match execlaw_plugin_host::register_builtins(plugin_host.registry(), &db, now, tools) {
            Ok(landed) => tracing::info!(count = landed.len(), "skill tools registered"),
            Err(e) => return Err(anyhow::anyhow!("register skill tools failed: {e}")),
        }
    }

    // 2026-05-03 — Phase B: attach the same shared SkillStore to
    // the plugin host so `install` imports plugin-shipped skills
    // (with `<plugin_id>/` namespace prepending) and `uninstall`
    // archives them. `attach_skill_store` is `OnceLock`-backed and
    // composes after `hydrate()` without disturbing already-loaded
    // subprocesses or script plugins.
    plugin_host.attach_skill_store(skill_store.clone());

    // 2026-06-01 — Graphify built-in tool. Gives the model a
    // first-class local entrypoint for graph generation/query so it
    // doesn't hallucinate a missing "graphify" toolkit command.
    // Registered before tool_access sync so Settings -> Tools gets a
    // seeded policy row on boot.
    {
        let now = chrono::Utc::now().timestamp();
        let tools = execlaw_server::graphify_tool::graphify_tools();
        match execlaw_plugin_host::register_builtins(plugin_host.registry(), &db, now, tools) {
            Ok(landed) => tracing::info!(count = landed.len(), "graphify tool registered"),
            Err(e) => return Err(anyhow::anyhow!("register graphify tool failed: {e}")),
        }
        // Keep existing deployments in sync with the widened graphify
        // default visibility. `register_builtins` preserves operator
        // policy by design, but graphify shipped initially as
        // Controller-only and would otherwise stay invisible to some
        // model trust classes forever.
        {
            let store = execlaw_core::tool_access::ToolAccessStore::new(&db);
            let allowed = vec![
                "Controller".to_owned(),
                "Delegated".to_owned(),
                "KnownTrusted".to_owned(),
                "KnownLimited".to_owned(),
                "UnknownPending".to_owned(),
            ];
            match store.set_policy("graphify", true, &allowed) {
                Ok(true) => tracing::info!("graphify tool policy ensured"),
                Ok(false) => {
                    tracing::warn!("graphify tool policy update skipped; tool row missing")
                }
                Err(e) => tracing::warn!(error = %e, "graphify tool policy update failed"),
            }
        }
    }

    // 2026-06-01 — Graphiti built-in tool bridge. Keeps temporal-memory
    // integration on the same tool-access/policy rails as every other
    // executable surface.
    {
        let now = chrono::Utc::now().timestamp();
        let tools = execlaw_server::graphiti_tool::graphiti_tools(db.clone());
        match execlaw_plugin_host::register_builtins(plugin_host.registry(), &db, now, tools) {
            Ok(landed) => tracing::info!(count = landed.len(), "graphiti tool registered"),
            Err(e) => return Err(anyhow::anyhow!("register graphiti tool failed: {e}")),
        }
        {
            let store = execlaw_core::tool_access::ToolAccessStore::new(&db);
            let allowed = vec!["Controller".to_owned()];
            match store.set_policy("graphiti", true, &allowed) {
                Ok(true) => tracing::info!("graphiti tool policy ensured"),
                Ok(false) => {
                    tracing::warn!("graphiti tool policy update skipped; tool row missing")
                }
                Err(e) => tracing::warn!(error = %e, "graphiti tool policy update failed"),
            }
        }
    }

    // 2026-06-02 — Wiki lifecycle built-in tool (Phase 1). Provides
    // ingest/compile/query/lifecycle operations for
    // `.obsidian/wiki/topics` without requiring plugin-specific
    // runtime wiring.
    {
        let now = chrono::Utc::now().timestamp();
        let tools = execlaw_server::wiki_lifecycle_tool::wiki_lifecycle_tools();
        match execlaw_plugin_host::register_builtins(plugin_host.registry(), &db, now, tools) {
            Ok(landed) => tracing::info!(count = landed.len(), "wiki_lifecycle tool registered"),
            Err(e) => return Err(anyhow::anyhow!("register wiki_lifecycle tool failed: {e}")),
        }
        {
            let store = execlaw_core::tool_access::ToolAccessStore::new(&db);
            let allowed = vec![
                "Controller".to_owned(),
                "Delegated".to_owned(),
                "KnownTrusted".to_owned(),
                "KnownLimited".to_owned(),
            ];
            match store.set_policy("wiki_lifecycle", true, &allowed) {
                Ok(true) => tracing::info!("wiki_lifecycle tool policy ensured"),
                Ok(false) => {
                    tracing::warn!("wiki_lifecycle tool policy update skipped; tool row missing")
                }
                Err(e) => tracing::warn!(error = %e, "wiki_lifecycle tool policy update failed"),
            }
        }
    }

    // 2026-06-02 — tool-chain phase 2 runtime (persisted plans/runs
    // + approval halt/resume). The plugin manifest declares
    // `host_implemented = true` for these names; dispatch lands on
    // these builtins while plugin enable/disable remains the
    // coarse ON/OFF switch in Settings -> Plugins.
    {
        let now = chrono::Utc::now().timestamp();
        let tools = execlaw_server::tool_chain_tool::tool_chain_tools(db.clone());
        match execlaw_plugin_host::register_builtins(plugin_host.registry(), &db, now, tools) {
            Ok(landed) => tracing::info!(count = landed.len(), "tool-chain tools registered"),
            Err(e) => return Err(anyhow::anyhow!("register tool-chain tools failed: {e}")),
        }
    }

    // Phase 8a: reflect every built-in + persisted plugin tool into
    // `config_tool_access` so the per-tool trust-class allowlist gate
    // has a row for everything. Idempotent — operator policy from
    // previous boots is preserved; only first-sight tools get the
    // open default.
    {
        let now = chrono::Utc::now().timestamp();
        match execlaw_server::tool_sync::sync_tool_access(&db, &plugin_host, now) {
            Ok(n) => tracing::info!(rows_synced = n, "tool_access sync complete"),
            Err(e) => {
                tracing::warn!(error = %e, "tool_access sync failed; dispatch gate will fall back to allow until next sync")
            }
        }
    }

    // Phase 7e: build the WebAuthn relying-party from EXECLAW_WEBAUTHN_*
    // env vars. Falling back to localhost:3031 keeps local-dev working
    // out of the box; production must set these to the real public
    // origin (HTTPS only — webauthn-rs rejects http origins outside
    // of `localhost`).
    let webauthn = build_webauthn_from_env(&config.bind_addr).map(std::sync::Arc::new);

    // Phase 8c: MCP connection manager. `reconcile()` spins up one
    // tokio actor per `enabled = true, transport = stdio` row in
    // `config_mcp_servers`, opens the connection, runs the
    // initialise handshake, and reflects every discovered tool
    // into `config_tool_access`.
    let mcp_host = execlaw_server::mcp_host::McpHost::new(db.clone());
    {
        let mh = mcp_host.clone();
        tokio::spawn(async move { mh.reconcile().await });
    }

    let events = execlaw_server::EventBus::new();

    // Phase 12.C — supervisor for managed inference backends. Best-
    // effort connect to the local Docker daemon; if it fails (no
    // Docker, e.g. dev on a host without Docker installed) we fall
    // through to `None` and managed-mode rows just sit `Stopped`
    // until Docker is available. The actual `run()` task is spawned
    // below alongside the other sweepers so it shares `sweep_stop`.
    //
    // Phase 14.C — when the supervisor IS wired, we also stand up
    // a host-side HuggingFace downloader pointed at
    // `~/.execlaw/hf-cache`. The supervisor blocks every managed
    // row's spawn behind a cache check + (if missing) a download,
    // surfacing real progress in the SPA pill and avoiding the
    // "container redownloads 18 GB on every CrashLoop" failure
    // mode that filled the user's disk on first run.
    // Connect to Docker once + share the controller across every
    // supervisor that needs it (backend + sidecar today; future
    // ones land here too). `None` when Docker is unreachable —
    // each supervisor below independently checks + degrades to
    // disabled mode rather than failing the whole boot.
    let docker_ctrl: Option<std::sync::Arc<dyn execlaw_container_manager::ServiceController>> =
        match execlaw_container_manager::BollardServiceController::connect_with_provenance(
            db.clone(),
        ) {
            Ok(ctrl) => Some(std::sync::Arc::new(ctrl)),
            Err(e) => {
                tracing::warn!("container supervisors disabled — Docker daemon unreachable: {e}");
                None
            }
        };

    // Phase 14.G (Apple Silicon plan) — the backend supervisor needs
    // a controller that can dispatch to either Docker (vLLM, Whisper,
    // Kokoro — every existing managed preset) OR a native subprocess
    // (Ollama on Apple Silicon, where Metal has no container
    // passthrough). When Docker is reachable, we wrap both behind
    // `MultiplexedServiceController` so the supervisor's existing
    // `Arc<dyn ServiceController>` slot keeps working unchanged.
    // When Docker is unreachable but we're on a Mac (or any host with
    // Ollama installed), we still expose the native path so the
    // wizard's Apple preset can spawn — Docker rows on the same host
    // will surface a clear "BollardServiceController cannot spawn..."
    // error rather than silently disappearing.
    let native_ctrl: std::sync::Arc<dyn execlaw_container_manager::ServiceController> =
        std::sync::Arc::new(execlaw_container_manager::NativeServiceController::new());
    let backend_ctrl: Option<std::sync::Arc<dyn execlaw_container_manager::ServiceController>> =
        match &docker_ctrl {
            Some(d) => Some(std::sync::Arc::new(
                execlaw_container_manager::MultiplexedServiceController::new(
                    d.clone(),
                    native_ctrl.clone(),
                ),
            )),
            None => {
                // Native-only path. Useful on Macs without Docker Desktop
                // installed — the operator can still configure the
                // Apple-Silicon Ollama preset and the supervisor will
                // spawn it. Sidecar supervisor (signal-cli etc.) stays
                // gated on `docker_ctrl` below so it correctly reports
                // "Docker unreachable" without affecting the inference
                // path.
                tracing::info!(
                    "backend supervisor falling back to native-only controller — Docker is \
                 unreachable, but managed-mode Apple-Silicon Ollama presets will still spawn"
                );
                Some(native_ctrl.clone())
            }
        };

    let backend_supervisor = backend_ctrl.as_ref().map(|ctrl| {
        // Resolve the host's primary HF cache directory.
        // Operator can override with EXECLAW_HF_CACHE; otherwise
        // keep it beside the configured database. A Windows service
        // may report the system profile as its home directory, while
        // the database still lives under the interactive operator's
        // profile; deriving the cache from the DB keeps Docker mounts
        // in the same accessible tree.
        let primary_cache: std::path::PathBuf = match std::env::var("EXECLAW_HF_CACHE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => db_path
                .parent()
                .map(|dir| dir.join("hf-cache"))
                .unwrap_or_else(|| std::path::PathBuf::from("./.execlaw-hf-cache")),
        };
        if let Err(e) = std::fs::create_dir_all(primary_cache.join("hub")) {
            tracing::warn!(
                path = %primary_cache.display(),
                "failed to create host HF cache directory: {e}"
            );
        }
        // Operator-supplied secondary caches live in
        // `config_general.hf_secondary_caches_json`. We snapshot
        // them at boot time; changing the list requires a
        // service restart for the supervisor to pick up. (Future
        // work: dynamic reload via `BackendSupervisor::reload_hf_caches()`.)
        let secondaries = execlaw_core::general_settings::GeneralSettingsStore::new(&db)
            .read_secondary_hf_caches()
            .unwrap_or_default();
        let token = std::env::var("HF_TOKEN").ok();
        let downloader =
            execlaw_container_manager::HfDownloader::new(primary_cache.clone(), secondaries, token);
        execlaw_server::backend_supervisor::BackendSupervisor::new(db.clone(), ctrl.clone())
            .with_hf_downloader(downloader, primary_cache)
    });

    // Phase 2b — sidecar supervisor. Manages every plugin-declared
    // companion container (`[services.sidecar]`). When Docker is
    // unreachable, leave it as `None` and the `/api/admin/sidecars`
    // route returns 503 with a friendly hint.
    //
    // Construction is cheap (just an Arc + a HashMap); we wire it
    // up regardless of whether any plugin has registered a sidecar
    // yet so the supervisor's snapshot is ready the moment a
    // plugin install lands.
    let sidecar_supervisor = docker_ctrl.as_ref().map(|ctrl| {
        execlaw_server::sidecar_supervisor::SidecarSupervisor::new(
            ctrl.clone(),
            plugin_host.registry().clone(),
        )
    });

    let voice_sessions = execlaw_server::voice_session::VoiceSessionRegistry::new(events.clone());

    // Phase 13.C — voice runtime resolves Whisper / Kokoro endpoints
    // from `config_backends` and the voice id from `config_personality`
    // on every new session. A Backends or Personality save mid-
    // conversation takes effect on the next utterance (mirrors
    // InferenceResolver). All wiring lives in
    // `voice_runtime::build_with_db` so it's exercised by unit tests
    // — this cli crate has no tests of its own.
    let voice_runtime =
        execlaw_server::voice_runtime::VoiceRuntime::build_with_db(events.clone(), db.clone());

    // Phase 16 — per-principal-group runner supervisor. Default
    // ON. Operators who want the legacy in-process chat path (or
    // who run on a Docker-less host where supervised spawn would
    // fail anyway) can opt out with `EXECLAW_RUNNERS_ENABLED=0`.
    //
    // We also defensively disable when:
    //   * Docker is unreachable (operator may not have started
    //     Docker Desktop yet), OR
    //   * the runner image isn't built (first-run on a fresh
    //     checkout — operator runs `docker build -f Dockerfile.runner
    //     -t execlaw/runner:dev .` once).
    // Either case logs a warning and falls through to in-process
    // chat so the operator isn't stranded.
    let runners_enabled = std::env::var("EXECLAW_RUNNERS_ENABLED")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    let runner_image =
        std::env::var("EXECLAW_RUNNER_IMAGE").unwrap_or_else(|_| "execlaw/runner:dev".to_owned());
    let (runner_supervisor, runner_launcher) = if runners_enabled {
        // Pull the trait into scope so `launcher.image_present`
        // resolves; the inherent method we want lives behind the
        // trait, not on `BollardRunnerLauncher` directly.
        use execlaw_server::runner_spawn::RunnerLauncher as _;
        match execlaw_server::runner_spawn::BollardRunnerLauncher::new_with_provenance(db.clone()) {
            Ok(launcher) => {
                // 2026-05-02 — autobuild the runner image when the
                // current control-plane binary is newer than the
                // image (or the image is missing). Operators
                // restart the control plane to pick up new code;
                // the runner is part of that surface and shouldn't
                // need a separate `docker build` step they have to
                // remember. Best-effort — production deployments
                // without source on disk fall through to the old
                // "warn + disable" branch when no Dockerfile is
                // findable.
                //
                // 2026-05-19 — wrap the runner-image probe in a
                // hard timeout. `image_present` calls bollard's
                // `inspect_image`, which awaits a Docker daemon
                // response with no built-in timeout. When Docker
                // Desktop is in a half-broken state (running but
                // not serving — common under WSL2 + WSL-integration
                // distros) the await stalls for ~120 seconds before
                // bollard's TCP read times out. That delay used to
                // gate the entire server boot, so the SPA spun on
                // the setup-wizard's docker check for two full
                // minutes. Capping at 5s means a healthy host pays
                // ~50-200 ms (the inspect round-trip), an unhealthy
                // host pays 5s and falls through to the "runner
                // image not found locally" warning + disabled
                // supervisor — same end-state, fast.
                let runner_probe_timeout = std::time::Duration::from_secs(5);
                // Image compilation is a separate operation and can take
                // minutes. Keeping it inside the five-second daemon probe
                // falsely disabled runners even when the build succeeded.
                let probe_result = tokio::time::timeout(
                    runner_probe_timeout,
                    launcher.image_present(&runner_image),
                )
                .await;
                let image_present = match probe_result {
                    Ok(_) => {
                        let _ = ensure_runner_image_fresh(&runner_image).await;
                        match tokio::time::timeout(
                            runner_probe_timeout,
                            launcher.image_present(&runner_image),
                        )
                        .await
                        {
                            Ok(present) => present,
                            Err(_) => {
                                tracing::warn!(
                                    image = %runner_image,
                                    timeout_secs = runner_probe_timeout.as_secs(),
                                    "runner image inspect timed out after refresh"
                                );
                                false
                            }
                        }
                    }
                    Err(_) => {
                        tracing::warn!(
                            image = %runner_image,
                            timeout_secs = runner_probe_timeout.as_secs(),
                            "runner image probe timed out — Docker daemon appears \
                             unresponsive. Disabling runner supervisor; restart Docker \
                             Desktop and re-launch execlaw to re-enable."
                        );
                        false
                    }
                };
                if image_present {
                    tracing::info!(
                        image = %runner_image,
                        "runner supervisor enabled"
                    );
                    // Build the spec template the supervisor will
                    // use for every lazy spawn (and for the
                    // controller prewarm). `group_id` +
                    // `spawn_secret_hex` get filled in per-spawn
                    // by `ensure_runner`; everything else is
                    // reused.
                    let rpc_url_template = std::env::var("EXECLAW_RPC_URL").unwrap_or_else(|_| {
                        format!("ws://host.docker.internal:{}", config.bind_addr.port())
                    });
                    let runner_network = std::env::var("EXECLAW_RUNNER_NETWORK").ok();
                    let spec_template = execlaw_server::runner_spawn::RunnerSpec {
                        group_id: String::new(),
                        image: runner_image.clone(),
                        spawn_secret_hex: String::new(),
                        rpc_url: rpc_url_template,
                        // Filled per-turn by ensure_runner from
                        // the resolved inference URL. We seed a
                        // sensible default here so a runner
                        // spawned before the chat path's per-turn
                        // override can still answer health checks.
                        inference_url: "http://host.docker.internal:8101/v1".into(),
                        memory_bytes: Some(2 * 1024 * 1024 * 1024),
                        network: runner_network,
                        env: vec![("RUST_LOG".into(), "info,execlaw_runner=debug".into())],
                    };
                    let launcher_arc = std::sync::Arc::new(launcher)
                        as std::sync::Arc<dyn execlaw_server::runner_spawn::RunnerLauncher>;
                    let supervisor = execlaw_server::runner_supervisor::RunnerSupervisor::new(
                        db.clone(),
                        events.clone(),
                    )
                    .with_launcher(launcher_arc.clone(), spec_template);
                    (Some(supervisor), Some(launcher_arc))
                } else {
                    tracing::warn!(
                        image = %runner_image,
                        "runner image not found locally; supervisor disabled. \
                         Build it once with: docker build -f Dockerfile.runner \
                         -t execlaw/runner:dev . (or override via \
                         EXECLAW_RUNNER_IMAGE=...)"
                    );
                    (None, None)
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Docker unreachable; runner supervisor disabled. \
                     Falling back to in-process chat path. Set \
                     EXECLAW_RUNNERS_ENABLED=0 to silence this warning."
                );
                (None, None)
            }
        }
    } else {
        tracing::info!("runner supervisor disabled via EXECLAW_RUNNERS_ENABLED=0");
        (None, None)
    };

    // Construct the research supervisor BEFORE AppState so the
    // admin endpoints (which carry an `AppState` clone) can reach
    // its `cancel_tokens` registry. C6c — this is what closes the
    // gap where the cancel admin endpoint flipped the DB row but
    // the gather phase kept burning tokens.
    let research_workspace =
        execlaw_server::research::ResearchWorkspace::new(data_dir.join("research"));
    // Channel-keyed transport registry. Phase B refactor: just a
    // `channel → (plugin_id, icon)` lookup. Auto-bridge sites
    // (text-reply bridge, attachment fan-out, research-PDF
    // dispatch) consult it for the channel's owning plugin id +
    // dispatch via `plugin_host.call_tool("<channel>.send_message",
    // ...)` directly. No TransportApi adapter layer.
    let host_transports = {
        let mut reg = execlaw_server::transport_registry::HostTransportRegistry::new();
        const SIGNAL_MANIFEST: &str = include_str!("../../../plugins/signal/plugin.toml");
        let signal_icon = execlaw_plugin_sdk::manifest::PluginManifest::parse(SIGNAL_MANIFEST)
            .ok()
            .and_then(|m| m.transport.and_then(|t| t.icon))
            .unwrap_or_else(|| "phone".to_owned());
        reg.register(
            "signal",
            execlaw_server::transport_registry::ChannelInfo {
                plugin_id: "signal".into(),
                icon: signal_icon,
            },
        );
        const WHATSAPP_MANIFEST: &str = include_str!("../../../plugins/whatsapp/plugin.toml");
        let whatsapp_icon = execlaw_plugin_sdk::manifest::PluginManifest::parse(WHATSAPP_MANIFEST)
            .ok()
            .and_then(|m| m.transport.and_then(|t| t.icon))
            .unwrap_or_else(|| "whatsapp".to_owned());
        reg.register(
            "whatsapp",
            execlaw_server::transport_registry::ChannelInfo {
                plugin_id: "whatsapp".into(),
                icon: whatsapp_icon,
            },
        );
        const SLACK_MANIFEST: &str = include_str!("../../../plugins/slack/plugin.toml");
        let slack_icon = execlaw_plugin_sdk::manifest::PluginManifest::parse(SLACK_MANIFEST)
            .ok()
            .and_then(|m| m.transport.and_then(|t| t.icon))
            .unwrap_or_else(|| "slack".to_owned());
        reg.register(
            "slack",
            execlaw_server::transport_registry::ChannelInfo {
                plugin_id: "slack".into(),
                icon: slack_icon,
            },
        );
        const SMS_SOCKET_MANIFEST: &str = include_str!("../../../plugins/sms-socket/plugin.toml");
        let sms_socket_icon =
            execlaw_plugin_sdk::manifest::PluginManifest::parse(SMS_SOCKET_MANIFEST)
                .ok()
                .and_then(|m| m.transport.and_then(|t| t.icon))
                .unwrap_or_else(|| "phone".to_owned());
        reg.register(
            "sms",
            execlaw_server::transport_registry::ChannelInfo {
                plugin_id: "sms-socket".into(),
                icon: sms_socket_icon,
            },
        );
        const DISCORD_MANIFEST: &str = include_str!("../../../plugins/discord/plugin.toml");
        let discord_icon = execlaw_plugin_sdk::manifest::PluginManifest::parse(DISCORD_MANIFEST)
            .ok()
            .and_then(|m| m.transport.and_then(|t| t.icon))
            .unwrap_or_else(|| "discord".to_owned());
        reg.register(
            "discord",
            execlaw_server::transport_registry::ChannelInfo {
                plugin_id: "discord".into(),
                icon: discord_icon,
            },
        );
        tracing::info!(channels = reg.len(), "host-transport registry populated");
        reg
    };

    let research_supervisor = execlaw_server::research::ResearchSupervisor::new(
        db.clone(),
        inference.clone(),
        research_workspace.clone(),
        events.clone(),
    )
    .with_host_transports(Some(host_transports.clone()))
    .with_plugin_host(Some(plugin_host.clone()));

    // Phase C (2026-05-03) — auto-capture worker. The summarizer
    // talks to `BackendPurpose::Small` so the standard turn isn't
    // contended; the worker gates internally on
    // `config_skills.auto_capture_enabled` (default OFF) so an
    // operator who hasn't opted in never burns inference cycles.
    //
    // 2026-05-13 — no model_id parameter: the worker's
    // `InferenceSummarizer` reads `resolved.model_id` from the
    // same DB row that supplied the endpoint, so caching a model
    // string at construction time is no longer a drift source.
    let (skill_capture_sink, _skill_capture_handle) =
        execlaw_server::skill_capture_runtime::spawn_capture_worker(
            db.clone(),
            skill_store.clone(),
            inference.clone(),
        );
    let (memory_extract_sink, _memory_extract_handle) =
        execlaw_server::memory_extract_runtime::spawn_memory_extraction_worker(
            db.clone(),
            inference.clone(),
        );
    // Phase D.3 — reuse-update worker. Same shape; gates on
    // `config_skills.reuse_update_enabled` (default OFF).
    let (reuse_update_sink, _reuse_update_handle) =
        execlaw_server::skill_capture_runtime::spawn_reuse_update_worker(
            db.clone(),
            skill_store.clone(),
            inference.clone(),
        );
    // new-2 — offline skill optimizer. Built (not spawned) here;
    // `chats.rs` calls `maybe_optimize` in a background task at
    // turn-end for each closed skill invocation.
    let optimizer_worker = Some(
        execlaw_server::skill_capture_runtime::build_optimizer_worker(
            db.clone(),
            skill_store.clone(),
            inference.clone(),
        ),
    );

    // M1/M2/M3 of Automations — spawn the durable event bus before
    // constructing AppState so the dispatcher + poller are live
    // before the first ingress (webhook routes mount after this
    // point). The handler runs the automation matcher: for each
    // delivered event, it looks up enabled automations whose
    // trigger.kind matches, evaluates trigger.when predicates, and
    // executes the typed graph. M3 adds the `AskAgent` node, which
    // delegates to the `AutomationsAgentPool`. The pool wraps
    // `InferenceAgentInvoker` (real LLM via the inference resolver)
    // and bounds concurrency at the locked default (1). When no
    // inference backend is configured, AskAgent fails fast with
    // `NoLlmConfigured` rather than silently hanging.
    let automation_bus_stop = std::sync::Arc::new(tokio::sync::Notify::new());
    // M5 — shared inference metrics handle. Threaded into the
    // automations agent invoker (Automations consumer attribution)
    // and stored on AppState so the `/admin/inference` page reads
    // the same instance. Future call sites (chat / routines /
    // research) wire the same handle for cross-consumer slicing.
    let inference_metrics = execlaw_server::inference_metrics::InferenceMetrics::new();
    let automation_agent_pool =
        execlaw_server::automation_agent::AutomationsAgentPool::new(std::sync::Arc::new(
            execlaw_server::automation_agent::InferenceAgentInvoker::new_with_metrics(
                db.clone(),
                inference.clone(),
                inference_metrics.clone(),
            ),
        ));
    let (automation_bus, automation_bus_tasks) =
        execlaw_server::automation_bus::AutomationBus::spawn(
            db.clone(),
            execlaw_server::automation_runtime::build_handler(
                execlaw_server::automation_runtime::ExecutorContext::new(
                    db.clone(),
                    automation_agent_pool.clone(),
                    Some(plugin_host.clone()),
                ),
            ),
            automation_bus_stop.clone(),
        );

    let state = execlaw_server::AppState {
        db: db.clone(),
        // Stash the exact config we just opened with so the
        // factory-reset endpoint can close-and-rebuild at the same
        // path with the same encryption posture. See
        // `crates/core/src/db.rs::Database::rebuild_to_empty`.
        db_config: std::sync::Arc::new(db_config),
        config: config.clone(),
        signer,
        refresh_store,
        events: events.clone(),
        event_log_hmac_key: hmac_key,
        inference: inference.clone(),
        plugin_host,
        webauthn,
        mcp_host,
        backend_supervisor,
        sidecar_supervisor: sidecar_supervisor.clone(),
        host_transports,
        voice_sessions,
        voice_runtime,
        turn_cancel: execlaw_server::turn_cancel::TurnCancellationRegistry::new(),
        runner_supervisor: runner_supervisor.clone(),
        research_supervisor: Some(research_supervisor.clone()),
        memory_extract: memory_extract_sink,
        skill_capture: skill_capture_sink,
        reuse_update: reuse_update_sink,
        optimizer_worker,
        data_dir: data_dir.clone(),
        automation_bus,
        automation_agent_pool,
        // M5 — same handle as the automations invoker holds, so the
        // `/admin/inference` snapshot endpoint sees AskAgent calls.
        inference_metrics,
        // Login brute-force gate. Constructed fresh each boot;
        // state is not durable (an operator restart resets counters).
        login_limiter: execlaw_server::auth_rate_limit::LoginRateLimiter::new(),
    };
    match execlaw_core::agents::AgentStore::new(&state.db)
        .reconcile_interrupted_runs(chrono::Utc::now().timestamp())
    {
        Ok(recovered) if recovered > 0 => tracing::warn!(
            recovered_agent_runs = recovered,
            "requeued mailbox input from agent runs interrupted by restart"
        ),
        Ok(_) => {}
        Err(error) => tracing::error!(
            %error,
            "could not reconcile interrupted agent runs; inspect agent run history"
        ),
    }
    let mut outbox_drain = execlaw_server::transport_outbox::spawn(state.clone());
    match execlaw_server::chats::reconcile_idempotent_chat_requests(
        &state,
        chrono::Utc::now().timestamp(),
        1_000,
    ) {
        Ok((responses_rebuilt, requests_marked_unknown))
            if responses_rebuilt > 0 || requests_marked_unknown > 0 =>
        {
            tracing::info!(
                responses_rebuilt,
                requests_marked_unknown,
                "reconciled interrupted chat requests at startup"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "chat request startup reconciliation needs review"),
    }
    // We don't await `automation_bus_tasks` — letting the spawned
    // dispatcher + poller run for the process lifetime. The `stop`
    // notify is held by the same shutdown path that drives the rest
    // of the sweepers (`sweep_stop`); we link them below so a SIGTERM
    // drains everything together.
    match execlaw_server::turn_controls_admin::recover_pending_queued_controls(&state) {
        Ok(recovered) if recovered > 0 => tracing::info!(
            recovered_queued_controls = recovered,
            "recovered pending next-turn chat controls"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "queued chat-control recovery needs operator review"),
    }
    drop(automation_bus_tasks);

    // Phase B (channel-plugin surface): wire the host-capabilities
    // arc into the script engine NOW that AppState exists. The
    // four Rhai bindings (`sidecar_url`, `ws_subscribe`,
    // `host_route_inbound`, plus the helper plumbing) start
    // returning real results from this call onward; before this
    // they error cleanly with "host capabilities not wired."
    {
        let caps =
            execlaw_server::host_caps_impl::AppStateHostCapabilities::new(state.clone()).into_arc();
        if state.plugin_host.attach_host_capabilities(caps).is_err() {
            tracing::warn!("host_caps already attached — second wiring call ignored");
        } else {
            tracing::info!("script-tier host capabilities attached");
        }
    }

    // Phase-7 background workers — run for the lifetime of the
    // process. The sweepers carry their own intervals; the server
    // owns the stop signal so a SIGTERM can drain everything.
    let sweep_stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let graphiti_worker = execlaw_server::graphiti_worker::GraphitiWorker::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { graphiti_worker.run(stop).await });
    }
    let log_sweeper = execlaw_core::log_retention::LogRetentionSweeper::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { log_sweeper.run(stop).await });
    }
    let artifact_sweeper = execlaw_core::artifact_sweeper::ArtifactSweeper::new(
        db.clone(),
        vec![
            data_dir.join("tool-results"),
            data_dir.join("blobs"),
            execlaw_server::host_caps_impl::builtin_artifacts_root_path(),
        ],
    );
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { artifact_sweeper.run(stop).await });
    }
    // 2026-04-29 — event retention: deletes `state_events` rows past
    // the operator-configured `history_retention_days` window.
    // Pinned + ephemeral conversations are exempt (the latter is
    // owned by EphemeralSweeper). Reads the policy live each tick so
    // a Settings change takes effect within one cadence.
    let event_sweeper = execlaw_core::event_retention::EventRetentionSweeper::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { event_sweeper.run(stop).await });
    }
    let ephemeral_sweeper = execlaw_core::ephemeral_sweeper::EphemeralSweeper::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { ephemeral_sweeper.run(stop).await });
    }
    let memory_lifecycle_sweeper =
        execlaw_server::memory_lifecycle_sweeper::MemoryLifecycleSweeper::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { memory_lifecycle_sweeper.run(stop).await });
    }
    // Phase 7 hardening — keeps `state_refresh_tokens` from growing
    // without bound. Expired rows are already rejected at consume
    // time; this just trims the table on an hourly cadence.
    let refresh_sweeper = execlaw_core::refresh_tokens::RefreshTokenSweeper::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { refresh_sweeper.run(stop).await });
    }
    // Phase 9 — OAuth proactive token refresh + pending-CSRF GC for
    // every plugin-configured `[[oauth_accounts]]` entry. Runs every
    // 60 s; refreshes tokens within 10 min of expiry; purges
    // expired authorize-flow CSRF rows. No-op when no clients are
    // configured.
    let oauth_sweeper = execlaw_server::oauth_sweeper::OauthSweeper::new(db.clone());
    {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { oauth_sweeper.run(stop).await });
    }
    // Phase 10 + 11.C — wall-clock-aligned cron tick that fires due
    // routines. Dispatch routes through chats::dispatch_routine_turn
    // so a routine fire is behaviourally identical to the controller
    // typing the prompt manually. Falls back to stub turn when no
    // inference backend is wired. See MIGRATION_PLAN §5.6.3.
    let _routine_runner = execlaw_server::routine_runner::spawn(state.clone());

    // Always-on child-agent supervisor. Definitions, mailbox messages,
    // runs, and checkpoints live in SQLite; the process task is only
    // the wake-up and execution mechanism and can be recreated safely.
    let agent_supervisor = execlaw_server::agent_supervisor::AgentSupervisor::new(
        db.clone(),
        inference.clone(),
        state.events.clone(),
        state.event_log_hmac_key.clone(),
    )
    .with_app_state(state.clone());
    let _agent_supervisor = agent_supervisor.spawn();

    // C3 — research subsystem supervisor. Picks up `Pending` rows
    // from `state_research_jobs`, claims them atomically, and spawns
    // a per-job runner that drives plan / gather / synthesize.
    // Workspace dir defaults to `~/.execlaw/research/`. Model id
    // mirrors the chat path's default; per-purpose routing lands
    // when the runner grows modality-aware backend selection.
    {
        let stop = sweep_stop.clone();
        let supervisor = research_supervisor.clone();
        tokio::spawn(async move { supervisor.run(stop).await });

        // C6 — research-retention sweeper. Purges terminal rows
        // past the global `history_retention_days` cutoff and
        // removes their workspace dirs. Hourly tick by default.
        let retention_sweeper = execlaw_server::research::ResearchRetentionSweeper::new(
            state.db.clone(),
            research_workspace,
        );
        let stop = sweep_stop.clone();
        tokio::spawn(async move { retention_sweeper.run(stop).await });

        // 2026-05-03 (rev 7) — clarification listener. Subscribes
        // to UiEvent::ResearchAwaitingInput and wakes the agent in
        // the affected conversation so it can relay the planner's
        // question to the user. Replaces the polling-on-
        // research_start fast path; no shutdown signal needed —
        // the task exits cleanly when the event-bus subscriber
        // returns Closed at server teardown.
        let _clarification_listener =
            execlaw_server::research::clarification_listener::spawn(state.clone());
    }

    // Phase 10 closure — purge state_routine_runs rows past the
    // 90-day retention window every hour. Mirrors the existing
    // log/ephemeral/refresh sweepers. Pending rows are preserved
    // regardless of age (a crashed mid-fire row stays visible).
    {
        let stop = sweep_stop.clone();
        let routine_run_sweeper =
            execlaw_core::routine_run_retention::RoutineRunRetentionSweeper::new(db.clone());
        tokio::spawn(async move { routine_run_sweeper.run(stop).await });
    }

    // M1 of Automations — retention sweep for `state_bus_events`.
    // Only dispatched rows are eligible (the sweeper's underlying
    // store call enforces this); pending rows are preserved
    // regardless of age so a stuck dispatcher stays visible.
    // 2-hour cadence matches `EventRetentionSweeper`.
    {
        let stop = sweep_stop.clone();
        let bus_event_sweeper =
            execlaw_core::bus_event_retention::BusEventRetentionSweeper::new(db.clone());
        tokio::spawn(async move { bus_event_sweeper.run(stop).await });
    }
    // M4 of Automations — daily sweep that populates
    // `state_automation_suggestions`. Groups recent bus events by
    // (kind, source), surfaces high-volume patterns that have no
    // matching enabled automation, and skips muted patterns.
    // The landing page reads from this table; agent-drafted
    // suggestions (M5) plug in at the same seam.
    {
        let stop = sweep_stop.clone();
        let sugg_sweeper =
            execlaw_server::automation_suggestions_sweeper::AutomationSuggestionsSweeper::new(
                db.clone(),
            );
        tokio::spawn(async move { sugg_sweeper.run(stop).await });
    }
    // Link the automation bus's dispatcher + poller into the same
    // shutdown signal as the sweepers — a SIGTERM drains the bus
    // alongside everything else.
    {
        let stop = sweep_stop.clone();
        let bus_stop = automation_bus_stop.clone();
        tokio::spawn(async move {
            stop.notified().await;
            bus_stop.notify_waiters();
        });
    }

    // Phase 12.C — backend supervisor reconcile loop. Only spawns
    // if the Docker connect succeeded above; otherwise managed-mode
    // backends are inert and the SPA shows a "Docker unreachable"
    // notice on the Backends page status pill.
    if let Some(sup) = state.backend_supervisor.clone() {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { sup.run(stop).await });
    }

    // Phase 2b — sidecar supervisor's reconcile loop. Same
    // start-only-if-Some pattern as backend_supervisor; on a
    // Docker-less host this is a no-op and the SPA's Sidecars
    // page reports the 503.
    if let Some(sup) = state.sidecar_supervisor.clone() {
        let stop = sweep_stop.clone();
        tokio::spawn(async move { sup.run(stop).await });
    }

    // Phase B lifecycle: fire each script plugin's optional
    // `on_enable()` Rhai hook. The sidecar supervisor was just
    // spawned above, so a transport plugin's WS-subscribe call
    // sees a live supervisor when it looks up `sidecar_url`.
    // Plugins whose sidecars are still spinning up handle the
    // None case gracefully (sidecar_url returns None → on_enable
    // logs + bails; the WS subscription ends up missing for that
    // boot — operator restart fixes it). A future tightening
    // would wait for sidecar healthy before firing, but that
    // adds blocking I/O to the boot path.
    {
        let plugin_host = state.plugin_host.clone();
        let state_for_wire = state.clone();
        let db_for_wire = db.clone();
        tokio::spawn(async move {
            // Small delay so the supervisor's first reconcile
            // pass has a chance to publish ports. Capped — if
            // sidecars aren't up by then we still fire on_enable
            // and let the plugin handle the None.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            plugin_host.fire_on_enable_for_all().await;

            // 2026-05-18 — Phase 8 wiring for the python-sandbox
            // plugin. Constructs the PythonSandboxService against
            // the kernel-gateway sidecar's published port and
            // registers the four python.* tools as host-implemented
            // builtins. No-op when the plugin isn't installed or
            // the sidecar isn't healthy (warning logged from inside).
            //
            // Service is held in a `static` so its OutputWatcher
            // (notify OS thread + tokio timer) stays alive for the
            // server's lifetime. Drop happens on process exit.
            if let Some(sup) = state_for_wire.sidecar_supervisor.as_ref() {
                let now = chrono::Utc::now().timestamp();
                match execlaw_server::python_sandbox::wire_python_sandbox(
                    sup,
                    plugin_host.registry(),
                    &db_for_wire,
                    &state_for_wire.events,
                    now,
                )
                .await
                {
                    Ok(Some(svc)) => {
                        // Stash in the server-crate's process-wide
                        // OnceLock so:
                        //   1. Drop doesn't run mid-server (anchor
                        //      for the OutputWatcher's threads).
                        //   2. Request handlers can reach it via
                        //      `python_sandbox::service()` — the
                        //      delete-thread handler uses this to
                        //      clean up `/work/<convo>/` on
                        //      conversation delete.
                        execlaw_server::python_sandbox::set_service(svc);
                    }
                    Ok(None) => {
                        // wire helper already logged the reason.
                    }
                    Err(e) => {
                        tracing::warn!(
                            ?e,
                            "python_sandbox wiring failed; python.* tools unavailable this boot"
                        );
                    }
                }
            }
        });
    }

    // Boot reconcile pass — merges any stale UnknownPending
    // principals shadowing a "My identities" mapping that was
    // added after the first cold-contact for that handle. Cheap
    // (single principals scan) and idempotent; safe to run on
    // every boot.
    match execlaw_server::principal_admit::reconcile_against_my_identities(&state.db) {
        Ok(report) if !report.merged.is_empty() => {
            tracing::info!(
                merged = report.merged.len(),
                bindings_repointed = report.bindings_repointed,
                conversations_repointed = report.conversations_repointed,
                "boot reconcile merged stale UnknownPending principals into canonical claimants",
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "boot reconcile failed; will retry on next add_my_identifier");
        }
    }

    // Phase B (signal v0.4.0+): the inbound consumer is now
    // plugin-owned. The signal plugin's `on_enable()` Rhai hook
    // fires from `PluginHost::hydrate` and calls `ws_subscribe`
    // against the supervised sidecar's `/v1/receive/<number>`
    // endpoint. The host gets out of the way — no spawn here.

    // Phase 13.D — voice-session reaper. Drops idle voice sessions
    // (operator closed the tab mid-mic) every REAP_INTERVAL so the
    // registry doesn't accumulate ghost entries. Both the
    // VoiceSessionRegistry and VoiceRuntime are passed in so future
    // versions can sweep both maps in lockstep.
    execlaw_server::voice_reaper::spawn(
        state.voice_sessions.clone(),
        state.voice_runtime.clone(),
        sweep_stop.clone(),
    );

    // Phase 16 — runner-supervisor reaper + controller prewarm.
    // Both opt-in via `runner_supervisor.is_some()`. Reaper sweeps
    // every REAP_INTERVAL (60s by default), wipes idle non-
    // controller runners' workspace volumes, and runs the per-turn
    // max-duration watchdog. Prewarm fires once on boot to spawn
    // the controller's runner so the first chat doesn't pay
    // cold-start latency.
    let mut startup_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    if let (Some(sup), Some(launcher)) = (runner_supervisor.as_ref(), runner_launcher.as_ref()) {
        let reaper_sup = sup.clone();
        let reaper_launcher = launcher.clone();
        let stop = sweep_stop.clone();
        startup_tasks.push(tokio::spawn(async move {
            tracing::info!(
                interval_secs = execlaw_server::runner_supervisor::REAP_INTERVAL.as_secs(),
                ttl_secs = execlaw_server::runner_supervisor::IDLE_TTL.as_secs(),
                max_turn_secs = execlaw_server::runner_supervisor::MAX_TURN_DURATION.as_secs(),
                "runner supervisor reaper running",
            );
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(
                        execlaw_server::runner_supervisor::REAP_INTERVAL,
                    ) => {
                        let _ = reaper_sup
                            .reap_idle_with_launcher(reaper_launcher.as_ref())
                            .await;
                        reaper_sup.watchdog_pass().await;
                    }
                    _ = stop.notified() => {
                        tracing::info!("runner supervisor reaper stopping");
                        return;
                    }
                }
            }
        }));

        // Boot orphan sweep: remove runner workspace volumes
        // whose principal group rows are gone (server crash mid-
        // reap, or operator deleted a group). Best-effort; logs
        // and continues on failure.
        let sweep_sup = sup.clone();
        let sweep_launcher = launcher.clone();
        tokio::spawn(async move {
            sweep_sup.boot_orphan_sweep(sweep_launcher.as_ref()).await;
        });

        // Prewarm the controller's runner. The first time anyone
        // chats with the controller we DON'T want a 1-3s cold
        // spawn delay; the supervisor blocks idle-reap on
        // controller groups by policy so this runner stays hot
        // until shutdown.
        let prewarm_sup = sup.clone();
        let prewarm_launcher = launcher.clone();
        let prewarm_db = db.clone();
        let prewarm_inference = state.inference.clone();
        let prewarm_bind_port = config.bind_addr.port();
        startup_tasks.push(tokio::spawn(async move {
            // Wait briefly so the WS endpoint is up before the
            // runner phones home. (Axum's `serve` task hasn't
            // necessarily started by the time we get here.)
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let inference_url = match prewarm_inference.resolve(
                &prewarm_db,
                execlaw_core::backends::BackendPurpose::Standard,
            ) {
                Some(c) => c.endpoint.clone(),
                None => {
                    tracing::info!(
                        "prewarm skipped: no inference backend configured (controller runner will spawn lazily on first chat)"
                    );
                    return;
                }
            };

            let image = std::env::var("EXECLAW_RUNNER_IMAGE")
                .unwrap_or_else(|_| "execlaw/runner:dev".to_owned());
            let rpc_url = std::env::var("EXECLAW_RPC_URL")
                .unwrap_or_else(|_| format!("ws://host.docker.internal:{prewarm_bind_port}"));
            let network = std::env::var("EXECLAW_RUNNER_NETWORK").ok();

            let spec = execlaw_server::runner_spawn::RunnerSpec {
                group_id: String::new(), // filled in by ensure_runner
                image,
                spawn_secret_hex: String::new(), // filled in
                rpc_url,
                inference_url,
                memory_bytes: Some(2 * 1024 * 1024 * 1024),
                network,
                env: vec![("RUST_LOG".into(), "info,execlaw_runner=debug".into())],
            };

            // The web SPA's send_message resolves an absent
            // `sender_principal_id` to the literal string
            // "controller" (see chats::resolve_sender). The chat
            // route's resolve_chat_group then hashes
            // `["controller"]` into the principal-set hash for
            // group `(web, {controller})`. Mirror that exactly so
            // the prewarmed group_id matches the one the chat
            // path will look up on the first send.
            match prewarm_sup
                .prewarm_controller(
                    prewarm_launcher.as_ref(),
                    "controller",
                    spec,
                    std::time::Duration::from_secs(30),
                )
                .await
            {
                Ok(handle) => {
                    tracing::info!(
                        group_id = %handle.group_id,
                        "controller runner prewarmed"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "controller prewarm failed (will spawn lazily on first chat)");
                }
            }
        }));
    }

    let _safe_chat_recovery = execlaw_server::chats::spawn_safe_chat_run_recovery(state.clone());
    let shutdown_state = state.clone();
    let app = execlaw_server::routes::build_router(state);
    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "execlaw server listening");
    // 2026-06-02: use into_make_service_with_connect_info so the
    // login handler can extract the peer SocketAddr for per-IP
    // rate limiting via axum::extract::ConnectInfo.
    let mut server_task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await
    });
    let serve_result: anyhow::Result<()> = tokio::select! {
        result = &mut server_task => join_server_result(result, "before drain"),
        _ = &mut shutdown_signal_task => {
            match tokio::time::timeout(SHUTDOWN_DRAIN_DEADLINE, &mut server_task).await {
                Ok(result) => join_server_result(result, "while draining"),
                Err(_) => {
                    tracing::error!(
                        drain_deadline_secs = SHUTDOWN_DRAIN_DEADLINE.as_secs(),
                        "server drain deadline expired; interrupting remaining requests for startup recovery"
                    );
                    server_task.abort();
                    Ok(())
                }
            }
        }
    };
    shutdown_signal_task.abort();
    let _ = outbox_drain.stop.send(true);
    sweep_stop.notify_waiters();
    for mut startup_task in startup_tasks {
        if tokio::time::timeout(std::time::Duration::from_secs(35), &mut startup_task)
            .await
            .is_err()
        {
            startup_task.abort();
            let _ = startup_task.await;
        }
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), &mut outbox_drain.task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "outbox drain task failed during shutdown"),
        Err(_) => {
            tracing::warn!(
                "outbox drain exceeded shutdown budget; in-flight leases will reconcile after restart"
            );
            outbox_drain.task.abort();
        }
    }
    let teardown = async {
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            shutdown_state.plugin_host.fire_on_disable_for_all(),
        )
        .await
        {
            Ok(fired) => tracing::info!(
                plugin_disable_hooks = fired,
                "plugin shutdown hooks completed"
            ),
            Err(_) => tracing::warn!(
                "plugin shutdown hooks exceeded their deadline; continuing process teardown"
            ),
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            shutdown_state.plugin_host.shutdown_runtime(),
        )
        .await
        {
            Ok(stopped) => {
                tracing::info!(stopped_plugin_runtimes = stopped, "plugin runtimes stopped")
            }
            Err(_) => tracing::error!("plugin runtimes did not stop before the shutdown deadline"),
        }
        if let Some(supervisor) = shutdown_state.sidecar_supervisor.as_ref() {
            let stopped = supervisor.stop_all().await;
            tracing::info!(
                stopped_sidecars = stopped,
                "sidecars stopped for service shutdown"
            );
        }
        if let Some(supervisor) = shutdown_state.backend_supervisor.as_ref() {
            let stopped = supervisor.stop_all().await;
            tracing::info!(
                stopped_backends = stopped,
                "managed backends stopped for service shutdown"
            );
        }
        if let Some(supervisor) = shutdown_state.runner_supervisor.as_ref() {
            let stopped = supervisor.shutdown_all().await;
            tracing::info!(
                stopped_runners = stopped,
                "conversation runners stopped for service shutdown"
            );
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(20), teardown)
        .await
        .is_err()
    {
        tracing::error!(
            "owned process teardown exceeded shutdown budget; remaining leases and containers require startup reconciliation"
        );
    }
    serve_result
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Hold the tracing-appender guard for the whole process lifetime
    // so the background flush thread sees every event before exit.
    // The editor adapter reserves stdout for Content-Length JSON-RPC frames.
    let _tracing_guard = if matches!(
        &cli.command,
        Command::Client {
            op: ClientOp::EditorAdapter { .. }
        }
    ) {
        None
    } else {
        init_tracing()
    };
    // 2026-05-16 — install a panic hook that emits a structured
    // tracing event (full backtrace + payload + location) and
    // aborts. The abort produces a core dump if the host's
    // `ulimit -c` allows; `rust-gdb <execlaw> <core>` then
    // attaches for post-mortem analysis. Without this hook a
    // panic in a tokio worker thread silently prints to stderr
    // and the server keeps running with a corrupt runtime —
    // exactly the failure mode that's hardest to debug.
    install_panic_hook();
    let result: anyhow::Result<()> = (|| match cli.command {
        Command::Install {
            no_encrypt,
            system,
            skip_migrate,
            bind,
            db,
        } => cmd_install(no_encrypt, system, skip_migrate, bind, db),
        Command::Service { op } => match op {
            ServiceOp::Install { system, bind, db } => service::install(system, bind, db),
            ServiceOp::Start { system } => service::start(system),
            ServiceOp::Stop { system } => service::stop(system),
            ServiceOp::Restart { system } => service::restart(system),
            ServiceOp::Status { system } => service::status(system),
            ServiceOp::Uninstall { system } => service::uninstall(system),
            ServiceOp::Run {
                bind,
                db,
                no_encrypt,
            } => {
                // The Windows path bootstraps its own tokio runtime
                // because StartServiceCtrlDispatcher returns BEFORE
                // we can establish one. The non-Windows path just
                // forwards into cmd_serve.
                #[cfg(windows)]
                {
                    service::windows_runtime_run(
                        bind,
                        db.unwrap_or_else(default_db_path),
                        no_encrypt,
                    )
                }
                #[cfg(not(windows))]
                {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()?;
                    rt.block_on(cmd_serve(
                        bind,
                        db.unwrap_or_else(default_db_path),
                        no_encrypt,
                        false,
                        None,
                    ))
                }
            }
        },
        Command::Doctor => cmd_doctor(),
        Command::Db { op } => match op {
            DbOp::EncryptPlaintext {
                db,
                backup,
                i_understand_execlaw_is_stopped,
            } => cmd_db_encrypt_plaintext(db, backup, i_understand_execlaw_is_stopped),
            DbOp::Migrate { db, no_encrypt } => {
                cmd_db_migrate(db.unwrap_or_else(default_db_path), no_encrypt)
            }
            DbOp::Status { db, no_encrypt } => {
                cmd_db_status(db.unwrap_or_else(default_db_path), no_encrypt)
            }
            DbOp::RepairChecksum { id, db, no_encrypt } => {
                cmd_db_repair_checksum(id, db.unwrap_or_else(default_db_path), no_encrypt)
            }
        },
        Command::Hw { op } => match op {
            HwOp::Rescan => cmd_hw_rescan(),
        },
        Command::Client { op } => match op {
            ClientOp::EditorAdapter { server } => api_client::run_editor_adapter(server),
            other => api_client::run(other),
        },
        Command::QualifyModel {
            db,
            no_encrypt,
            context_tokens,
        } => cmd_qualify_model(
            db.unwrap_or_else(default_db_path),
            no_encrypt,
            context_tokens,
        ),
        Command::Serve {
            bind,
            db,
            no_encrypt,
            allow_unsigned_local_development,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(cmd_serve(
                bind,
                db.unwrap_or_else(default_db_path),
                no_encrypt,
                allow_unsigned_local_development,
                None,
            ))
        }
        Command::Replay {
            conversation_id,
            at,
            db,
            no_encrypt,
        } => cmd_replay(
            conversation_id,
            at,
            db.unwrap_or_else(default_db_path),
            no_encrypt,
        ),
        Command::Eval { op } => match op {
            EvalOp::Flag {
                conversation_id,
                range,
                label,
                tags,
                notes,
                db,
                no_encrypt,
            } => cmd_eval_flag(
                conversation_id,
                range,
                label,
                tags,
                notes,
                db.unwrap_or_else(default_db_path),
                no_encrypt,
            ),
            EvalOp::List {
                label,
                db,
                no_encrypt,
            } => cmd_eval_list(label, db.unwrap_or_else(default_db_path), no_encrypt),
            EvalOp::ExportFlagged {
                id,
                to,
                redaction_map,
                consent,
                incident_ref,
                release_ref,
                tool_catalog_snapshots,
                synthetic_media,
                policy_cases,
                db,
                no_encrypt,
            } => cmd_eval_export_flagged(
                id,
                EvalExportOptions {
                    output_path: to,
                    redaction_map_path: redaction_map,
                    consent,
                    incident_ref,
                    release_ref,
                    tool_catalog_snapshots_path: tool_catalog_snapshots,
                    synthetic_media_path: synthetic_media,
                    policy_cases_path: policy_cases,
                    db_path: db.unwrap_or_else(default_db_path),
                    no_encrypt,
                },
            ),
        },
        Command::Memory { op } => match op {
            MemoryOp::ExportAssertion {
                assertion_id,
                to,
                redaction_map,
                consent,
                db,
                no_encrypt,
            } => cmd_memory_export_assertion(
                assertion_id,
                to,
                redaction_map,
                consent,
                db.unwrap_or_else(default_db_path),
                no_encrypt,
            ),
        },
        Command::BackfillEvents { db, no_encrypt } => {
            cmd_backfill_events(db.unwrap_or_else(default_db_path), no_encrypt)
        }
        Command::ResignEvents {
            db,
            no_encrypt,
            i_understand_history_will_be_resigned,
        } => cmd_resign_events(
            db.unwrap_or_else(default_db_path),
            no_encrypt,
            i_understand_history_will_be_resigned,
        ),
        Command::Backup { to, db, no_encrypt } => {
            cmd_backup(to, db.unwrap_or_else(default_db_path), no_encrypt)
        }
        Command::RotateKeys {
            backup,
            db,
            i_understand_database_key_will_change,
        } => cmd_rotate_keys(
            backup,
            db.unwrap_or_else(default_db_path),
            i_understand_database_key_will_change,
        ),
        Command::Restore {
            from,
            db,
            force,
            no_encrypt,
        } => cmd_restore(from, db.unwrap_or_else(default_db_path), force, no_encrypt),
        Command::DeleteThread {
            conversation_id,
            i_understand_this_deletes_history,
            db,
            no_encrypt,
        } => cmd_delete_thread(
            conversation_id,
            i_understand_this_deletes_history,
            db.unwrap_or_else(default_db_path),
            no_encrypt,
        ),
        Command::ListThreads { db, no_encrypt } => {
            cmd_list_threads(db.unwrap_or_else(default_db_path), no_encrypt)
        }
    })();

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            // {:#} prints the full anyhow chain (top context + every
            // wrapped source separated by `: `). Without it the user
            // only sees the outermost `with_context` message, which
            // for service-install hides the underlying SCM error.
            eprintln!("execlaw: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sqlcipher")]
    #[test]
    fn plaintext_database_conversion_preserves_data_and_verified_backup() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let database_path = dir.path().join("execlaw.db");
        let backup_path = dir.path().join("execlaw-before-encryption.db");
        let key = [0x17_u8; 32];

        let plaintext = execlaw_core::Database::open(&execlaw_core::DbConfig {
            path: database_path.clone(),
            key: None,
        })
        .unwrap();
        plaintext
            .with_conn(|connection| {
                connection.execute_batch(
                    "CREATE TABLE preserved(value TEXT NOT NULL); \
                     INSERT INTO preserved(value) VALUES ('state survives');",
                )?;
                Ok(())
            })
            .unwrap();
        drop(plaintext);

        encrypt_plaintext_database(&database_path, &backup_path, &key)
            .unwrap_or_else(|error| panic!("plaintext conversion failed: {error:#}"));

        let mut encrypted_header = [0_u8; 16];
        std::fs::File::open(&database_path)
            .unwrap()
            .read_exact(&mut encrypted_header)
            .unwrap();
        assert_ne!(&encrypted_header, b"SQLite format 3\0");
        let mut backup_header = [0_u8; 16];
        std::fs::File::open(&backup_path)
            .unwrap()
            .read_exact(&mut backup_header)
            .unwrap();
        assert_eq!(&backup_header, b"SQLite format 3\0");

        let encrypted = execlaw_core::Database::open(&execlaw_core::DbConfig {
            path: database_path,
            key: Some(execlaw_core::db::SqlCipherKey::RawBytes(key.to_vec())),
        })
        .unwrap();
        let value = encrypted
            .with_conn(|connection| {
                Ok(
                    connection.query_row("SELECT value FROM preserved", [], |row| {
                        row.get::<_, String>(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(value, "state survives");
    }

    #[test]
    fn client_send_accepts_repeatable_task_contract_flags() {
        let cli = Cli::try_parse_from([
            "execlaw",
            "client",
            "send",
            "--conversation-id",
            "conv-1",
            "--text",
            "run the task",
            "--acceptance-criterion",
            "tests=Focused tests pass",
            "--acceptance-criterion",
            "review=Review findings are fixed",
            "--optional-acceptance-criterion",
            "latency=Under the preferred latency target",
            "--required-artifact",
            "report=Test report",
            "--delivery-required",
        ])
        .unwrap();
        let Command::Client {
            op:
                ClientOp::Send {
                    acceptance_criteria,
                    optional_acceptance_criteria,
                    required_artifacts,
                    delivery_required,
                    ..
                },
        } = cli.command
        else {
            panic!("expected client send command");
        };
        assert_eq!(acceptance_criteria.len(), 2);
        assert_eq!(
            optional_acceptance_criteria,
            ["latency=Under the preferred latency target"]
        );
        assert_eq!(required_artifacts, ["report=Test report"]);
        assert!(delivery_required);
    }

    #[test]
    fn client_send_accepts_saved_run_resume_without_replacement_text() {
        let cli = Cli::try_parse_from([
            "execlaw",
            "client",
            "send",
            "--conversation-id",
            "conv-1",
            "--resume-run-id",
            "turn:conv-1:9",
            "--request-id",
            "resume-attempt-1",
        ])
        .unwrap();
        let Command::Client {
            op:
                ClientOp::Send {
                    text,
                    resume_run_id,
                    ..
                },
        } = cli.command
        else {
            panic!("expected client send command");
        };
        assert!(text.is_empty());
        assert_eq!(resume_run_id.as_deref(), Some("turn:conv-1:9"));
    }

    #[test]
    fn flagged_fixture_export_accepts_incident_and_release_links() {
        let cli = Cli::try_parse_from([
            "execlaw",
            "eval",
            "export-flagged",
            "7",
            "--to",
            "fixture.json",
            "--redaction-map",
            "redaction.json",
            "--consent",
            "--incident-ref",
            "INC-42",
            "--release-ref",
            "v2026.09.29",
            "--tool-catalog-snapshots",
            "tool-catalogs.json",
            "--synthetic-media",
            "synthetic-media.json",
            "--policy-cases",
            "policy-cases.json",
        ])
        .unwrap();
        let Command::Eval {
            op:
                EvalOp::ExportFlagged {
                    incident_ref,
                    release_ref,
                    tool_catalog_snapshots,
                    synthetic_media,
                    policy_cases,
                    ..
                },
        } = cli.command
        else {
            panic!("expected eval export-flagged command");
        };
        assert_eq!(incident_ref.as_deref(), Some("INC-42"));
        assert_eq!(release_ref.as_deref(), Some("v2026.09.29"));
        assert_eq!(
            tool_catalog_snapshots.as_deref(),
            Some(std::path::Path::new("tool-catalogs.json"))
        );
        assert_eq!(
            synthetic_media.as_deref(),
            Some(std::path::Path::new("synthetic-media.json"))
        );
        assert_eq!(
            policy_cases.as_deref(),
            Some(std::path::Path::new("policy-cases.json"))
        );
    }

    #[test]
    fn fixture_export_reads_and_redacts_the_catalog_snapshot_for_the_flagged_turn() {
        let db =
            execlaw_core::Database::open(&execlaw_core::DbConfig::in_memory_unencrypted()).unwrap();
        execlaw_core::MigrationRunner::new(&db).apply_all().unwrap();
        let conversation_id = execlaw_core::ConversationId::from("catalog-export-fixture");
        execlaw_core::conversation::ConversationStore::new(&db)
            .upsert(&execlaw_core::conversation::ConversationRow {
                conversation_id: conversation_id.clone(),
                kind: execlaw_core::conversation::ConversationKind::ControllerDM,
                last_seq: execlaw_core::ids::EventSeq(0),
                phase: execlaw_core::conversation::Phase::Idle,
                controller_id: None,
                trust_class: "Controller".into(),
                snapshot_blob: None,
                snapshot_seq: None,
                lease_owner: None,
                lease_expires: None,
                modality: execlaw_core::conversation::Modality::Text,
                display_name: None,
                display_name_source: "auto".into(),
                is_pinned: false,
                is_ephemeral: false,
                ephemeral_expires_at: None,
                last_activity_at: 1,
                context_window_policy: None,
            })
            .unwrap();
        execlaw_core::events::EventLog::new(&db)
            .commit_turn(
                &conversation_id,
                execlaw_core::ids::EventSeq(0),
                vec![
                    execlaw_core::events::PendingEvent::encode(
                        execlaw_core::events::EventKind::UserMsg,
                        &serde_json::json!({"text":"fixture", "attachment_ids": []}),
                        Some("controller-fixture".into()),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        let runs = execlaw_core::runs::RunStore::new(&db);
        let run_id = runs
            .create_run(&execlaw_core::runs::NewRun {
                conversation_id: conversation_id.clone(),
                parent_run_id: None,
                input_event_seq: execlaw_core::EventSeq(1),
                started_at: 1,
                deadline_at: None,
            })
            .unwrap();
        let tool = serde_json::json!({
            "type":"function",
            "function":{
                "name":"fixture.echo",
                "description":"private schema detail",
                "parameters":{"type":"object","properties":{}}
            }
        });
        let snapshot = serde_json::json!({
            "tools":[tool.clone()],
            "discoverable_tools":[tool.clone()]
        });
        let pinned = vec![tool.clone(), tool.clone()];
        let manifest = execlaw_core::runs::RunInputManifest {
            input_version: 1,
            prompt_hash: "prompt-fixture".into(),
            model_settings_hash: "model-fixture".into(),
            tool_catalog_hash: execlaw_core::tool::tool_schema_hash(&serde_json::Value::Array(
                pinned,
            )),
            tool_catalog_snapshot_json: Some(serde_json::to_string(&snapshot).unwrap()),
            recorded_at: 1,
        };
        runs.record_input_manifest(&run_id, &manifest).unwrap();
        let mut redactor = EvalFixtureRedactor::new(EvalRedactionMap {
            replacements: vec![EvalRedactionReplacement {
                source: "private schema detail".into(),
                replacement: "<SYNTHETIC_SCHEMA_DESCRIPTION>".into(),
            }],
        })
        .unwrap();

        let exported = export_tool_catalog_for_turn(&db, &conversation_id, 1, &mut redactor)
            .unwrap()
            .unwrap();
        assert_eq!(exported.source_catalog_hash, manifest.tool_catalog_hash);
        assert_eq!(
            exported.tools[0]["function"]["description"],
            "<SYNTHETIC_SCHEMA_DESCRIPTION>"
        );
        assert!(
            !exported.tools[0]
                .to_string()
                .contains("private schema detail")
        );
        assert_eq!(exported.snapshot_sha256.len(), 64);

        execlaw_core::events::EventLog::new(&db)
            .commit_turn(
                &conversation_id,
                execlaw_core::ids::EventSeq(1),
                vec![
                    execlaw_core::events::PendingEvent::encode(
                        execlaw_core::events::EventKind::UserMsg,
                        &serde_json::json!({"text":"legacy fixture", "attachment_ids": []}),
                        Some("controller-fixture".into()),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        let legacy_run = runs
            .create_run(&execlaw_core::runs::NewRun {
                conversation_id: conversation_id.clone(),
                parent_run_id: None,
                input_event_seq: execlaw_core::ids::EventSeq(2),
                started_at: 2,
                deadline_at: None,
            })
            .unwrap();
        runs.record_input_manifest(
            &legacy_run,
            &execlaw_core::runs::RunInputManifest {
                input_version: 1,
                prompt_hash: "prompt-legacy".into(),
                model_settings_hash: "model-fixture".into(),
                tool_catalog_hash: manifest.tool_catalog_hash.clone(),
                tool_catalog_snapshot_json: None,
                recorded_at: 2,
            },
        )
        .unwrap();
        let sidecar_snapshot = serde_json::json!({
            "tools":snapshot["tools"].clone(),
            "discoverable_tools":snapshot["discoverable_tools"].clone(),
        });
        let mut legacy_catalogs = Vec::new();
        merge_tool_catalog_sidecars(
            &db,
            &conversation_id,
            &std::collections::HashSet::from([2]),
            &mut legacy_catalogs,
            vec![execlaw_core::eval::ToolCatalogFixture {
                turn_seq: 2,
                source_catalog_hash: manifest.tool_catalog_hash,
                snapshot_sha256: execlaw_core::harness::HarnessStore::fingerprint(
                    &sidecar_snapshot,
                )
                .unwrap(),
                tools: sidecar_snapshot["tools"].as_array().unwrap().clone(),
                discoverable_tools: sidecar_snapshot["discoverable_tools"]
                    .as_array()
                    .unwrap()
                    .clone(),
            }],
            &mut redactor,
        )
        .unwrap();
        assert_eq!(legacy_catalogs.len(), 1);
        assert_eq!(
            legacy_catalogs[0].tools[0]["function"]["description"],
            "<SYNTHETIC_SCHEMA_DESCRIPTION>"
        );
    }

    #[test]
    fn rotate_keys_requires_explicit_acknowledgement_before_touching_files() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("live.db");
        let backup = dir.path().join("backup.db");
        let error = cmd_rotate_keys(backup.clone(), database.clone(), false).unwrap_err();
        assert!(error.to_string().contains("event history remains signed"));
        assert!(!database.exists());
        assert!(!backup.exists());
    }

    #[test]
    fn backup_restore_roundtrip_validates_disposable_database() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let backup = dir.path().join("backup.db");
        let restored = dir.path().join("restored.db");
        let database = open_db(&source, true).unwrap();
        execlaw_core::migrations::MigrationRunner::new(&database)
            .apply_all()
            .unwrap();
        database
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE config_general SET bind_address = '127.0.0.1:3031', updated_at = 1 \
                     WHERE id = 1",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        drop(database);

        cmd_backup(backup.clone(), source, true).unwrap();
        verify_database_snapshot(&backup, true).unwrap();
        cmd_restore(backup, restored.clone(), false, true).unwrap();
        let restored_db = open_db(&restored, true).unwrap();
        let bind: String = restored_db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT bind_address FROM config_general WHERE id = 1",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(bind, "127.0.0.1:3031");
    }

    #[test]
    fn forced_restore_reapplies_newer_privacy_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.db");
        let backup = dir.path().join("before-deletion.db");
        let db = open_db(&live, true).unwrap();
        execlaw_core::migrations::MigrationRunner::new(&db)
            .apply_all()
            .unwrap();
        drop(db);

        cmd_backup(backup.clone(), live.clone(), true).unwrap();
        let live_db = open_db(&live, true).unwrap();
        live_db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_privacy_deletion_jobs \
                     (deletion_id, resource_kind, resource_id, requested_by, request_source, \
                      payload_json, status, requested_at, updated_at, completed_at) \
                     VALUES ('deleted-job', 'research_job', 'research-1', 'controller-1', \
                             'controller', '{}', 'complete', 10, 11, 11)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        drop(live_db);

        cmd_restore(backup, live.clone(), true, true).unwrap();
        let restored_db = open_db(&live, true).unwrap();
        let count = restored_db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM state_privacy_deletion_jobs \
                     WHERE resource_id = 'research-1' AND status = 'complete'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn resolve_bind_prefers_cli_over_db() {
        let (bind, src) = resolve_bind(Some("0.0.0.0:9000".into()), Some("127.0.0.1:3031".into()));
        assert_eq!(bind, "0.0.0.0:9000");
        assert_eq!(src, "cli");
    }

    #[test]
    fn resolve_bind_falls_back_to_db_when_no_cli() {
        let (bind, src) = resolve_bind(None, Some("0.0.0.0:8080".into()));
        assert_eq!(bind, "0.0.0.0:8080");
        assert_eq!(src, "config_general");
    }

    #[test]
    fn resolve_bind_falls_back_to_default_when_neither_provided() {
        let (bind, src) = resolve_bind(None, None);
        assert_eq!(bind, "127.0.0.1:3031");
        assert_eq!(src, "default");
    }

    #[test]
    fn resolve_bind_treats_blank_db_value_as_missing() {
        // Defensive — `config_general.bind_address` is NOT NULL in
        // the schema, but a future migration / hand-edit could leave
        // it as whitespace; bind to the safe loopback default rather
        // than passing `""` to TcpListener::bind.
        let (bind, src) = resolve_bind(None, Some("   ".into()));
        assert_eq!(bind, "127.0.0.1:3031");
        assert_eq!(src, "default");
    }

    #[tokio::test]
    async fn service_stop_is_forwarded_to_the_graceful_server_drain() {
        let (service_stop, service_rx) = tokio::sync::oneshot::channel();
        let (shutdown_trigger, mut trigger_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(trigger_shutdown(Some(service_rx), shutdown_trigger));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut trigger_rx)
                .await
                .is_err()
        );
        service_stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut trigger_rx)
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }
}
