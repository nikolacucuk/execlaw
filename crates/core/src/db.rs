//! SQLite + SQLCipher connection management.
//!
//! Per MIGRATION_PLAN §6.3 / §6.4:
//!
//! - Whole `execlaw.db` is SQLCipher-encrypted; master key comes from the OS
//!   keyring (or a passphrase-file fallback for headless hosts).
//! - Every connection runs `PRAGMA key = '...'` before any SQL.
//! - `PRAGMA journal_mode = WAL` so readers don't block the writer.
//! - `PRAGMA foreign_keys = ON` **per connection** (the default is OFF in
//!   SQLite and is easy to forget — so we enforce it here).
//! - `PRAGMA synchronous = FULL` for the main DB. The vault tables live in the same DB, so the whole
//!   file is encrypted; operator can re-key with `execlaw vault rekey`.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::Instant;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const DB_EXECUTION_QUEUE_CAPACITY: usize = 16;
type DbJob = Box<dyn FnOnce() + Send + 'static>;

struct DbExecutionMetricsInner {
    queued: AtomicUsize,
    running: AtomicUsize,
    rejected: AtomicU64,
    completed: AtomicU64,
    queue_wait_micros_total: AtomicU64,
    queue_wait_micros_max: AtomicU64,
    operation_micros_total: AtomicU64,
    operation_micros_max: AtomicU64,
    service_micros_total: AtomicU64,
    service_micros_max: AtomicU64,
    transaction_micros_total: AtomicU64,
    transaction_micros_max: AtomicU64,
}

impl DbExecutionMetricsInner {
    fn new() -> Self {
        Self {
            queued: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            rejected: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            queue_wait_micros_total: AtomicU64::new(0),
            queue_wait_micros_max: AtomicU64::new(0),
            operation_micros_total: AtomicU64::new(0),
            operation_micros_max: AtomicU64::new(0),
            service_micros_total: AtomicU64::new(0),
            service_micros_max: AtomicU64::new(0),
            transaction_micros_total: AtomicU64::new(0),
            transaction_micros_max: AtomicU64::new(0),
        }
    }

    fn record_duration(total: &AtomicU64, max: &AtomicU64, elapsed_micros: u64) {
        total.fetch_add(elapsed_micros, Ordering::Relaxed);
        max.fetch_max(elapsed_micros, Ordering::Relaxed);
    }
}

struct DbExecutionService {
    sender: mpsc::SyncSender<DbJob>,
    metrics: Arc<DbExecutionMetricsInner>,
}

static DB_EXECUTION_SERVICE: OnceLock<Result<DbExecutionService, String>> = OnceLock::new();

fn db_execution_service() -> Result<&'static DbExecutionService, DbError> {
    DB_EXECUTION_SERVICE
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<DbJob>(DB_EXECUTION_QUEUE_CAPACITY);
            let metrics = Arc::new(DbExecutionMetricsInner::new());
            let worker_metrics = Arc::clone(&metrics);
            std::thread::Builder::new()
                .name("execlaw-db-executor".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        worker_metrics.queued.fetch_sub(1, Ordering::Relaxed);
                        worker_metrics.running.fetch_add(1, Ordering::Relaxed);
                        let started = Instant::now();
                        // A bad caller closure must not terminate the shared
                        // process-wide worker and strand every queued DB job.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                        let elapsed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
                        DbExecutionMetricsInner::record_duration(
                            &worker_metrics.service_micros_total,
                            &worker_metrics.service_micros_max,
                            elapsed,
                        );
                        worker_metrics.completed.fetch_add(1, Ordering::Relaxed);
                        worker_metrics.running.fetch_sub(1, Ordering::Relaxed);
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(DbExecutionService { sender, metrics })
        })
        .as_ref()
        .map_err(|error| DbError::ExecutorUnavailable(error.clone()))
}

/// Snapshot of the bounded database executor and SQLite operation timings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DbExecutionMetrics {
    pub queued_jobs: usize,
    pub running_jobs: usize,
    pub rejected_jobs: u64,
    pub completed_jobs: u64,
    pub queue_wait_micros_total: u64,
    pub queue_wait_micros_max: u64,
    pub operation_micros_total: u64,
    pub operation_micros_max: u64,
    pub service_micros_total: u64,
    pub service_micros_max: u64,
    pub transaction_micros_total: u64,
    pub transaction_micros_max: u64,
}

/// Main database and SQLite side-file sizes at one sample time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DbFileSizes {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub journal_bytes: u64,
}

/// Progress reported by a non-blocking SQLite WAL checkpoint attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct WalCheckpointProgress {
    pub checkpoint_blocked: bool,
    pub frames_in_wal: i64,
    pub frames_checkpointed: i64,
}

/// Configuration for opening the database.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// Path to the SQLCipher-encrypted DB file.
    pub path: PathBuf,
    /// Raw master key. If `None`, the DB is opened as **plaintext SQLite**
    /// (only permitted for unit tests — production always has a key).
    ///
    /// Expected shape for production: a hex-encoded 32-byte key — rusqlite
    /// accepts it via `PRAGMA key = "x'...'";`. For simplicity the caller
    /// can also pass a passphrase and we'll use it directly.
    pub key: Option<SqlCipherKey>,
}

impl DbConfig {
    /// Config for an ephemeral in-memory database used by unit tests.
    /// SQLCipher-key-less — acceptable ONLY in `#[cfg(test)]`.
    pub fn in_memory_unencrypted() -> Self {
        Self {
            path: PathBuf::from(":memory:"),
            key: None,
        }
    }
}

/// Key material for SQLCipher.
///
/// Prefer `RawBytes(32)` in production (hex-escaped when we invoke `PRAGMA`).
/// `Passphrase` is offered as a convenience for the headless-fallback flow
/// described in §6.4.
#[derive(Clone)]
pub enum SqlCipherKey {
    /// 32 raw bytes. Will be hex-escaped in the PRAGMA statement.
    RawBytes(Vec<u8>),
    /// Human-entered passphrase (SQLCipher derives the key via PBKDF2).
    Passphrase(String),
}

impl std::fmt::Debug for SqlCipherKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SqlCipherKey::RawBytes(b) => f
                .debug_tuple("RawBytes")
                .field(&format!("<{} bytes>", b.len()))
                .finish(),
            SqlCipherKey::Passphrase(_) => {
                f.debug_tuple("Passphrase").field(&"<redacted>").finish()
            }
        }
    }
}

impl Drop for SqlCipherKey {
    fn drop(&mut self) {
        match self {
            Self::RawBytes(bytes) => bytes.zeroize(),
            Self::Passphrase(passphrase) => passphrase.zeroize(),
        }
    }
}

/// Top-level errors from this crate's database layer.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database configuration error: {0}")]
    Config(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event log invariant violated: {0}")]
    Invariant(String),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("event log tamper detected: {0}")]
    TamperDetected(String),
    #[error("database execution queue is full; retry shortly")]
    Backpressure,
    #[error("database execution worker is unavailable: {0}")]
    ExecutorUnavailable(String),
    #[error("database execution operation panicked")]
    WorkerPanicked,
}

/// Synchronous stores retain the existing API. Long async database work
/// should use [`Database::run_blocking`], which has one FIFO worker and a
/// bounded queue. SQLite mutations remain serialized on the shared connection;
/// no read pool is enabled without benchmark evidence.
#[derive(Clone)]
pub struct Database {
    inner: Arc<std::sync::Mutex<Connection>>,
    path: PathBuf,
    event_hmac_key: Arc<std::sync::RwLock<Option<Zeroizing<Vec<u8>>>>>,
}

impl Database {
    /// Open (or create) the database at the given config.
    ///
    /// Applies `PRAGMA key` (when `key` is Some), `journal_mode = WAL`,
    /// `foreign_keys = ON`, and `synchronous = FULL`.
    pub fn open(config: &DbConfig) -> Result<Self, DbError> {
        #[cfg(not(feature = "sqlcipher"))]
        if config.key.is_some() {
            return Err(DbError::Config(
                "encrypted database requires a SQLCipher-enabled binary".into(),
            ));
        }

        let conn = if config.path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            if let Some(parent) = config.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Connection::open(&config.path)?
        };

        Self::apply_init_pragmas(&conn, config)?;

        Ok(Self {
            inner: Arc::new(std::sync::Mutex::new(conn)),
            path: config.path.clone(),
            event_hmac_key: Arc::new(std::sync::RwLock::new(None)),
        })
    }

    fn apply_init_pragmas(conn: &Connection, config: &DbConfig) -> Result<(), DbError> {
        // Key BEFORE anything else — SQLCipher requires the first statement
        // on a new connection to set the key, otherwise reads return corrupt
        // data.
        if let Some(key) = &config.key {
            match key {
                SqlCipherKey::RawBytes(bytes) => {
                    if bytes.len() != 32 {
                        return Err(DbError::Config(format!(
                            "expected 32 raw key bytes, got {}",
                            bytes.len()
                        )));
                    }
                    let hex = Zeroizing::new(hex::encode(bytes));
                    // Use `x'...'` blob literal form so SQLCipher treats this
                    // as raw key material and skips KDF.
                    let pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex.as_str()));
                    conn.execute_batch(&pragma)?;
                }
                SqlCipherKey::Passphrase(pass) => {
                    // rusqlite offers `pragma_update` but it double-quotes the
                    // value; we need single-quote escaping here to match
                    // SQLCipher's expected input.
                    let escaped = Zeroizing::new(pass.replace('\'', "''"));
                    let pragma = Zeroizing::new(format!("PRAGMA key = '{}';", escaped.as_str()));
                    conn.execute_batch(&pragma)?;
                }
            }

            #[cfg(feature = "sqlcipher")]
            {
                let version: String = conn
                    .query_row("PRAGMA cipher_version", [], |row| row.get(0))
                    .map_err(|_| {
                        DbError::Config("SQLCipher support is unavailable in this binary".into())
                    })?;
                if version.is_empty() {
                    return Err(DbError::Config(
                        "SQLCipher support is unavailable in this binary".into(),
                    ));
                }
            }
        }

        // WAL — readers don't block the writer.
        conn.pragma_update(None, "journal_mode", "WAL")?;

        // Foreign keys default OFF in SQLite; force ON every connection.
        conn.pragma_update(None, "foreign_keys", "ON")?;

        // FULL syncs the WAL at each commit. Durability still depends on the
        // storage stack honoring flush requests.
        conn.pragma_update(None, "synchronous", "FULL")?;

        Ok(())
    }

    /// Path the DB was opened at (for log/display purposes).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sample the database and side-file sizes without following symlinks.
    pub fn file_sizes(&self) -> DbFileSizes {
        let size = |path: PathBuf| {
            std::fs::symlink_metadata(path)
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        };
        DbFileSizes {
            database_bytes: size(self.path.clone()),
            wal_bytes: size(sibling_with_suffix(&self.path, "-wal")),
            shm_bytes: size(sibling_with_suffix(&self.path, "-shm")),
            journal_bytes: size(sibling_with_suffix(&self.path, "-journal")),
        }
    }

    /// Attempt a passive WAL checkpoint and report progress without waiting
    /// for readers to finish. SQLite may leave frames for a later retry.
    pub fn checkpoint_passive(&self) -> Result<WalCheckpointProgress, DbError> {
        self.with_conn(|connection| {
            let (checkpoint_blocked, frames_in_wal, frames_checkpointed) =
                connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
            Ok(WalCheckpointProgress {
                checkpoint_blocked: checkpoint_blocked != 0,
                frames_in_wal,
                frames_checkpointed,
            })
        })
    }

    /// Read WAL checkpoint progress without performing maintenance work.
    pub fn wal_checkpoint_status(&self) -> Result<WalCheckpointProgress, DbError> {
        self.with_conn(|connection| {
            let (checkpoint_blocked, frames_in_wal, frames_checkpointed) =
                connection.query_row("PRAGMA wal_checkpoint(NOOP)", [], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
            Ok(WalCheckpointProgress {
                checkpoint_blocked: checkpoint_blocked != 0,
                frames_in_wal,
                frames_checkpointed,
            })
        })
    }

    /// Report SQLite's per-connection synchronous setting (FULL is 2).
    pub fn synchronous_level(&self) -> Result<i64, DbError> {
        self.with_conn(|connection| {
            Ok(connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?)
        })
    }

    /// Attach the process's event signing key to all event-log views of this
    /// database, including background subsystems holding only a DB clone.
    pub fn set_event_hmac_key(&self, key: Vec<u8>) -> Result<(), DbError> {
        if key.is_empty() {
            return Err(DbError::Config("event HMAC key is empty".into()));
        }
        let mut slot = self
            .event_hmac_key
            .write()
            .map_err(|_| DbError::Config("event HMAC key lock poisoned".into()))?;
        if slot
            .as_ref()
            .is_some_and(|existing| existing.as_slice() != key.as_slice())
        {
            return Err(DbError::Config(
                "cannot replace the event HMAC key in a running database".into(),
            ));
        }
        *slot = Some(Zeroizing::new(key));
        Ok(())
    }

    pub(crate) fn event_hmac_key(&self) -> Option<Zeroizing<Vec<u8>>> {
        self.event_hmac_key.read().ok()?.clone()
    }

    /// Execute a closure holding the connection lock.
    ///
    /// Every caller *must* keep the closure short — we're single-writer.
    pub fn with_conn<F, R>(&self, f: F) -> Result<R, DbError>
    where
        F: FnOnce(&Connection) -> Result<R, DbError>,
    {
        let started = Instant::now();
        let guard = self
            .inner
            .lock()
            .map_err(|e| DbError::Config(format!("connection mutex poisoned: {e}")))?;
        let result = f(&guard);
        if let Ok(service) = db_execution_service() {
            let elapsed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            DbExecutionMetricsInner::record_duration(
                &service.metrics.operation_micros_total,
                &service.metrics.operation_micros_max,
                elapsed,
            );
        }
        result
    }

    /// Execute a transactional closure.
    pub fn transaction<F, R>(&self, f: F) -> Result<R, DbError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R, DbError>,
    {
        let started = Instant::now();
        let outcome = (|| {
            let mut guard = self
                .inner
                .lock()
                .map_err(|e| DbError::Config(format!("connection mutex poisoned: {e}")))?;
            let tx = guard.transaction()?;
            let result = f(&tx)?;
            tx.commit()?;
            Ok(result)
        })();
        if let Ok(service) = db_execution_service() {
            let elapsed = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            DbExecutionMetricsInner::record_duration(
                &service.metrics.transaction_micros_total,
                &service.metrics.transaction_micros_max,
                elapsed,
            );
        }
        outcome
    }

    /// Run blocking database work through the process-wide bounded FIFO
    /// executor. The worker queue holds at most 16 pending operations.
    /// Saturation returns [`DbError::Backpressure`] immediately. Accepted
    /// work continues if the requesting future is cancelled, so callers
    /// should use this for bounded database work with durable retry semantics.
    pub async fn run_blocking<T, F>(&self, operation: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let service = db_execution_service()?;
        let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
        let queued_at = Instant::now();
        let metrics = Arc::clone(&service.metrics);
        metrics.queued.fetch_add(1, Ordering::Relaxed);
        let job: DbJob = Box::new(move || {
            let queue_wait = queued_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
            DbExecutionMetricsInner::record_duration(
                &metrics.queue_wait_micros_total,
                &metrics.queue_wait_micros_max,
                queue_wait,
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
                .map_err(|_| DbError::WorkerPanicked);
            let _ = result_sender.send(result);
        });
        match service.sender.try_send(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                service.metrics.queued.fetch_sub(1, Ordering::Relaxed);
                service.metrics.rejected.fetch_add(1, Ordering::Relaxed);
                return Err(DbError::Backpressure);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                service.metrics.queued.fetch_sub(1, Ordering::Relaxed);
                return Err(DbError::ExecutorUnavailable("worker channel closed".into()));
            }
        }
        result_receiver
            .await
            .map_err(|_| DbError::ExecutorUnavailable("worker dropped an operation".into()))?
    }

    /// Read queueing, rejection, operation, and transaction metrics.
    pub fn execution_metrics(&self) -> DbExecutionMetrics {
        let Ok(service) = db_execution_service() else {
            return DbExecutionMetrics::default();
        };
        DbExecutionMetrics {
            queued_jobs: service.metrics.queued.load(Ordering::Relaxed),
            running_jobs: service.metrics.running.load(Ordering::Relaxed),
            rejected_jobs: service.metrics.rejected.load(Ordering::Relaxed),
            completed_jobs: service.metrics.completed.load(Ordering::Relaxed),
            queue_wait_micros_total: service
                .metrics
                .queue_wait_micros_total
                .load(Ordering::Relaxed),
            queue_wait_micros_max: service
                .metrics
                .queue_wait_micros_max
                .load(Ordering::Relaxed),
            operation_micros_total: service
                .metrics
                .operation_micros_total
                .load(Ordering::Relaxed),
            operation_micros_max: service.metrics.operation_micros_max.load(Ordering::Relaxed),
            service_micros_total: service.metrics.service_micros_total.load(Ordering::Relaxed),
            service_micros_max: service.metrics.service_micros_max.load(Ordering::Relaxed),
            transaction_micros_total: service
                .metrics
                .transaction_micros_total
                .load(Ordering::Relaxed),
            transaction_micros_max: service
                .metrics
                .transaction_micros_max
                .load(Ordering::Relaxed),
        }
    }

    /// Read a single PRAGMA value as text. Intended for sanity tests.
    pub fn pragma_text(&self, name: &str) -> Result<String, DbError> {
        self.with_conn(|c| {
            let mut got = String::new();
            c.pragma_query(None, name, |row| {
                got = row.get::<_, String>(0).unwrap_or_default();
                Ok(())
            })?;
            Ok(got)
        })
    }

    /// Re-encrypt an open SQLCipher database with a new raw 32-byte key.
    /// The caller must stage and verify a recoverable backup before calling
    /// this; keyring/file persistence is intentionally owned by the CLI.
    #[cfg(feature = "sqlcipher")]
    pub fn rekey_sqlcipher(&self, new_key: &[u8]) -> Result<(), DbError> {
        if new_key.len() != 32 {
            return Err(DbError::Config(format!(
                "expected 32 raw key bytes, got {}",
                new_key.len()
            )));
        }
        let hex = hex::encode(new_key);
        let pragma = format!("PRAGMA rekey = \"x'{hex}'\";");
        self.with_conn(|conn| {
            conn.execute_batch(&pragma)?;
            Ok(())
        })
    }

    /// Wipe the database back to first-boot state by closing the
    /// current connection, deleting the file(s) on disk, and opening
    /// a fresh blank one at the same path with the same encryption
    /// posture. **The schema is gone** — callers MUST run
    /// `MigrationRunner::apply_all()` afterwards to re-create tables
    /// and re-fire migration seeds.
    ///
    /// Why this is preferable to schema-level `DROP TABLE` loops:
    ///
    ///   * Virtual tables (FTS5 `skill_search`, etc.) require the
    ///     vtable module to load successfully at DROP time. A
    ///     mismatched SQLite/SQLCipher build, a missing extension, or
    ///     simply a connection that was opened with different module
    ///     registrations than at `CREATE VIRTUAL TABLE` time will
    ///     surface as `vtable constructor failed: <name>` — observed
    ///     in production today (2026-05-13). File-level delete bypasses
    ///     the vtable module entirely.
    ///   * `sqlite_master` enumeration + `DROP TABLE` is sensitive to
    ///     FK declaration order: SQLite refuses to drop a parent that
    ///     a child still references. Even with `foreign_keys = OFF`
    ///     some configurations trip on this. File delete sidesteps
    ///     the dependency graph.
    ///   * Plain semantic: "factory reset" should mean "the DB equals
    ///     what `execlaw install` would have produced." Deleting the
    ///     file then re-opening is exactly that.
    ///
    /// On `:memory:` paths the file-delete is skipped; the in-memory
    /// connection is simply replaced with a fresh in-memory one. Used
    /// by the test harness.
    ///
    /// Side-files cleaned up alongside the main DB file:
    ///
    ///   * `<path>-wal` — write-ahead log (always present in WAL mode)
    ///   * `<path>-shm` — shared-memory file backing the WAL
    ///   * `<path>-journal` — rollback journal (only present
    ///     transiently or under non-WAL configurations; try-delete
    ///     is harmless if absent)
    ///
    /// The caller must hold no other handles to the Database. The
    /// `Arc<Mutex<Connection>>` we own is the only handle by design
    /// (see the struct docstring), so the lock acquired below is
    /// sufficient. If a future refactor introduces a second
    /// connection, this method must be revisited.
    pub fn rebuild_to_empty(&self, config: &DbConfig) -> Result<(), DbError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| DbError::Config(format!("connection mutex poisoned: {e}")))?;

        // Replace the existing connection with a temporary in-memory
        // one. Dropping the old `Connection` releases its file
        // handle, which is required on Windows before `remove_file`
        // can succeed.
        *guard = Connection::open_in_memory()?;

        // `:memory:` path → nothing on disk to delete, just open a
        // new in-memory connection in place.
        let is_memory = self.path == Path::new(":memory:");
        if !is_memory {
            // Delete the main DB file + every SQLite side-file that
            // can sit alongside it. `remove_file` is best-effort
            // for the side-files (NotFound is fine — they may not
            // exist), but a failure on the main file is fatal
            // because callers depend on a fresh state.
            let path = &self.path;
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            for suffix in ["-wal", "-shm", "-journal"] {
                let side = sibling_with_suffix(path, suffix);
                if side.exists() {
                    // Side-file delete failure is non-fatal — SQLite
                    // will overwrite stale WAL/SHM on first checkpoint
                    // of the new connection. Log via stderr (we don't
                    // have a tracing handle in core).
                    if let Err(e) = std::fs::remove_file(&side) {
                        eprintln!(
                            "rebuild_to_empty: failed to remove {} (continuing): {e}",
                            side.display(),
                        );
                    }
                }
            }
            // Open fresh at the same path. `Connection::open` creates
            // the file if absent.
            let new_conn = Connection::open(path)?;
            Self::apply_init_pragmas(&new_conn, config)?;
            *guard = new_conn;
        } else {
            // For in-memory, the temporary we already swapped in IS
            // the new connection. Apply pragmas to it.
            Self::apply_init_pragmas(&guard, config)?;
        }

        Ok(())
    }
}

/// Compute the path of an SQLite side-file (e.g. `db-wal`, `db-shm`,
/// `db-journal`) given the main DB path. SQLite appends the suffix to
/// the full filename rather than swapping the extension — `foo.db`'s
/// WAL is `foo.db-wal`, not `foo-wal`.
fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_without_key_succeeds() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        db.with_conn(|c| {
            c.execute_batch("CREATE TABLE t (id INTEGER);")?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn foreign_keys_pragma_is_on() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        let val: i64 = db
            .with_conn(|c| {
                let v: i64 = c.query_row("PRAGMA foreign_keys;", [], |r| r.get(0))?;
                Ok(v)
            })
            .unwrap();
        assert_eq!(val, 1, "foreign_keys must be ON on every new connection");
    }

    #[test]
    fn wal_connections_use_full_synchronous_mode() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        assert_eq!(db.synchronous_level().unwrap(), 2);
    }

    #[test]
    fn passive_checkpoint_reports_frames_held_by_a_long_reader() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checkpoint-probe.db");
        let db = Database::open(&DbConfig {
            path: path.clone(),
            key: None,
        })
        .unwrap();
        db.with_conn(|connection| {
            connection.execute_batch(
                "CREATE TABLE checkpoint_probe (id INTEGER PRIMARY KEY, value BLOB NOT NULL)",
            )?;
            Ok(())
        })
        .unwrap();
        db.transaction(|transaction| {
            transaction.execute(
                "INSERT INTO checkpoint_probe (id, value) VALUES (1, zeroblob(4096))",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("PRAGMA journal_mode=WAL; BEGIN DEFERRED;")
            .unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM checkpoint_probe", [], |row| {
                row.get(0)
            })
            .unwrap();
        db.transaction(|transaction| {
            transaction.execute(
                "INSERT INTO checkpoint_probe (id, value) VALUES (2, zeroblob(4096))",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let progress = db.checkpoint_passive().unwrap();
        assert!(progress.frames_in_wal > 0);
        assert!(progress.frames_checkpointed < progress.frames_in_wal);
        reader.execute_batch("ROLLBACK;").unwrap();
        let resumed = db.checkpoint_passive().unwrap();
        assert_eq!(resumed.frames_checkpointed, resumed.frames_in_wal);
    }

    #[test]
    fn wal_status_probe_does_not_checkpoint_frames() {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::open(&DbConfig {
            path: directory.path().join("wal-status-probe.db"),
            key: None,
        })
        .unwrap();
        db.with_conn(|connection| {
            connection.execute_batch("CREATE TABLE wal_status_probe (value TEXT NOT NULL)")?;
            Ok(())
        })
        .unwrap();
        db.transaction(|transaction| {
            transaction.execute(
                "INSERT INTO wal_status_probe VALUES ('pending-checkpoint')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let reader = Connection::open(db.path()).unwrap();
        reader
            .execute_batch("PRAGMA journal_mode=WAL; BEGIN DEFERRED;")
            .unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM wal_status_probe", [], |row| {
                row.get(0)
            })
            .unwrap();
        db.transaction(|transaction| {
            transaction.execute(
                "INSERT INTO wal_status_probe VALUES ('after-reader-start')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let wal_before = db.file_sizes().wal_bytes;
        let status = db.wal_checkpoint_status().unwrap();
        assert!(status.frames_in_wal >= 1);
        assert!(status.frames_checkpointed < status.frames_in_wal);
        assert_eq!(db.wal_checkpoint_status().unwrap(), status);
        assert!(db.file_sizes().wal_bytes >= wal_before);
        reader.execute_batch("ROLLBACK;").unwrap();
    }

    #[test]
    fn injected_sqlite_full_preserves_committed_rows_and_allows_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::open(&DbConfig {
            path: directory.path().join("disk-full-probe.db"),
            key: None,
        })
        .unwrap();
        db.with_conn(|connection| {
            connection.execute_batch(
                "CREATE TABLE durable_marker (id INTEGER PRIMARY KEY, value TEXT NOT NULL); \
                 CREATE TABLE pending_effect_marker (id INTEGER PRIMARY KEY, status TEXT NOT NULL); \
                 CREATE TABLE growing_payload (value BLOB NOT NULL); \
                 INSERT INTO durable_marker VALUES (1, 'event-committed'); \
                 INSERT INTO pending_effect_marker VALUES (1, 'pending');",
            )?;
            let pages: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            connection.pragma_update(None, "max_page_count", pages + 2)?;
            Ok(())
        })
        .unwrap();

        let failure = db
            .transaction(|transaction| {
                for _ in 0..32 {
                    transaction.execute(
                        "INSERT INTO growing_payload(value) VALUES (zeroblob(8192))",
                        [],
                    )?;
                }
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(
            failure,
            DbError::Sqlite(rusqlite::Error::SqliteFailure(ref error, _))
                if error.code == rusqlite::ErrorCode::DiskFull
        ));

        let (event_value, effect_status): (String, String) = db
            .with_conn(|connection| {
                Ok((
                    connection.query_row(
                        "SELECT value FROM durable_marker WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )?,
                    connection.query_row(
                        "SELECT status FROM pending_effect_marker WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(event_value, "event-committed");
        assert_eq!(effect_status, "pending");

        db.with_conn(|connection| {
            connection.pragma_update(None, "max_page_count", 100_000)?;
            Ok(())
        })
        .unwrap();
        db.transaction(|transaction| {
            transaction.execute(
                "INSERT INTO growing_payload(value) VALUES (zeroblob(8192))",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let rows: i64 = db
            .with_conn(|connection| {
                Ok(connection
                    .query_row("SELECT COUNT(*) FROM growing_payload", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn committed_full_sync_wal_transaction_survives_process_termination() {
        const CHILD_PATH: &str = "EXECLAW_DB_CRASH_CHILD_PATH";
        if let Ok(path) = std::env::var(CHILD_PATH) {
            let db = Database::open(&DbConfig {
                path: PathBuf::from(path),
                key: None,
            })
            .unwrap();
            db.with_conn(|connection| {
                connection.execute_batch(
                    "CREATE TABLE durable_probe (id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
                )?;
                Ok(())
            })
            .unwrap();
            db.transaction(|transaction| {
                transaction.execute(
                    "INSERT INTO durable_probe (id, value) VALUES (1, 'committed')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
            std::process::abort();
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("crash-probe.db");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "db::tests::committed_full_sync_wal_transaction_survives_process_termination",
            ])
            .env(CHILD_PATH, &path)
            .status()
            .unwrap();
        assert!(!status.success(), "child must terminate before clean close");

        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        let value: String = reopened
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT value FROM durable_probe WHERE id = 1",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(value, "committed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_executor_returns_backpressure_at_queue_capacity() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let first_db = db.clone();
        let first = tokio::spawn(async move {
            first_db
                .run_blocking(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    0usize
                })
                .await
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            tokio::time::sleep(std::time::Duration::from_millis(10)),
        )
        .await
        .expect("a blocked database job must not block async workers");

        let mut queued = Vec::with_capacity(DB_EXECUTION_QUEUE_CAPACITY);
        for index in 0..DB_EXECUTION_QUEUE_CAPACITY {
            let queued_db = db.clone();
            queued.push(tokio::spawn(async move {
                queued_db.run_blocking(move || index).await
            }));
        }
        for _ in 0..10_000 {
            if db.execution_metrics().queued_jobs >= DB_EXECUTION_QUEUE_CAPACITY {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            db.execution_metrics().queued_jobs,
            DB_EXECUTION_QUEUE_CAPACITY
        );
        assert!(matches!(
            db.run_blocking(|| 99usize).await,
            Err(DbError::Backpressure)
        ));
        release_tx.send(()).unwrap();
        assert_eq!(first.await.unwrap().unwrap(), 0);
        for (index, task) in queued.into_iter().enumerate() {
            assert_eq!(task.await.unwrap().unwrap(), index);
        }

        db.with_conn(|connection| {
            connection.execute_batch(
                "CREATE TABLE executor_counter (value INTEGER NOT NULL); \
                 INSERT INTO executor_counter(value) VALUES (0);",
            )?;
            Ok(())
        })
        .unwrap();
        for _ in 0..2 {
            let mut updates = Vec::new();
            for _ in 0..8 {
                let update_executor = db.clone();
                let update_db = db.clone();
                updates.push(tokio::spawn(async move {
                    update_executor
                        .run_blocking(move || {
                            update_db.transaction(|transaction| {
                                transaction
                                    .execute("UPDATE executor_counter SET value = value + 1", [])?;
                                Ok(())
                            })
                        })
                        .await
                }));
            }
            for update in updates {
                update.await.unwrap().unwrap().unwrap();
            }
        }
        let updates: i64 = db
            .with_conn(|connection| {
                Ok(connection
                    .query_row("SELECT value FROM executor_counter", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(updates, 16);
        assert_eq!(db.execution_metrics().queued_jobs, 0);
    }

    #[tokio::test]
    async fn panicking_job_returns_error_without_stopping_shared_worker() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        let failure: Result<(), DbError> =
            db.run_blocking(|| panic!("injected worker panic")).await;
        assert!(matches!(failure, Err(DbError::WorkerPanicked)));
        assert_eq!(db.run_blocking(|| 42usize).await.unwrap(), 42);
        assert_eq!(db.execution_metrics().queued_jobs, 0);
    }

    #[cfg(not(feature = "sqlcipher"))]
    #[test]
    fn keyed_open_fails_closed_without_sqlcipher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("must-not-be-plaintext.db");
        let error = Database::open(&DbConfig {
            path: path.clone(),
            key: Some(SqlCipherKey::RawBytes(vec![0x71; 32])),
        })
        .unwrap_err();
        assert!(matches!(error, DbError::Config(_)));
        assert!(!path.exists());
    }

    #[cfg(feature = "sqlcipher")]
    #[test]
    fn file_backed_db_with_passphrase_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("execlaw.db");
        let cfg = DbConfig {
            path: path.clone(),
            key: Some(SqlCipherKey::Passphrase("hunter2-but-longer".into())),
        };
        let db = Database::open(&cfg).unwrap();
        db.with_conn(|c| {
            c.execute_batch(
                "CREATE TABLE sanity(id INTEGER PRIMARY KEY, label TEXT); \
                 INSERT INTO sanity(id, label) VALUES (1, 'ok');",
            )?;
            Ok(())
        })
        .unwrap();
        drop(db);

        // Re-open with the same key and read the row back.
        let db2 = Database::open(&cfg).unwrap();
        let label: String = db2
            .with_conn(|c| {
                let v: String =
                    c.query_row("SELECT label FROM sanity WHERE id = 1", [], |r| r.get(0))?;
                Ok(v)
            })
            .unwrap();
        assert_eq!(label, "ok");
    }

    #[cfg(feature = "sqlcipher")]
    #[test]
    fn wrong_passphrase_cannot_read_encrypted_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.db");
        let good = DbConfig {
            path: path.clone(),
            key: Some(SqlCipherKey::Passphrase(
                "correct horse battery staple".into(),
            )),
        };
        let db = Database::open(&good).unwrap();
        db.with_conn(|c| {
            c.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (42);")?;
            Ok(())
        })
        .unwrap();
        drop(db);

        let bad = DbConfig {
            path,
            key: Some(SqlCipherKey::Passphrase("totally different".into())),
        };
        let db2 = Database::open(&bad).unwrap();
        // Reading the table should fail because SQLCipher can't decrypt it.
        let result: Result<i64, _> =
            db2.with_conn(|c| Ok(c.query_row("SELECT x FROM t", [], |r| r.get(0))?));
        assert!(
            result.is_err(),
            "wrong passphrase should NOT yield a readable DB"
        );
    }

    #[cfg(feature = "sqlcipher")]
    #[test]
    fn encrypted_rotation_drill_preserves_backup_secrets_plugins_and_event_chain() {
        use crate::events::{EventKind, EventLog, KeyRing, PendingEvent};
        use crate::ids::{ConversationId, EventSeq};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotate.db");
        let backup_path = dir.path().join("rotate-before.db");
        let old_key = [0x21_u8; 32];
        let new_key = [0x73_u8; 32];
        let event_hmac_key = [0x45_u8; 32];
        let config = |key: [u8; 32], path: std::path::PathBuf| DbConfig {
            path,
            key: Some(SqlCipherKey::RawBytes(key.to_vec())),
        };
        let db = Database::open(&config(old_key, path.clone())).unwrap();
        crate::migrations::MigrationRunner::new(&db)
            .apply_all()
            .unwrap();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO vault_secrets (name, plugin_id, value_blob, created_at, updated_at) VALUES ('drill-secret', NULL, X'CAFE', 1, 1)", [])?;
            conn.execute("INSERT INTO state_plugins (plugin_id, version, manifest_toml, stage_path, enabled, installed_at, updated_at) VALUES ('drill-plugin', '1.0.0', 'id = \\\"drill-plugin\\\"', 'fixture', 1, 1, 1)", [])?;
            Ok(())
        }).unwrap();
        let cid = ConversationId::from("rotation-drill");
        let log = EventLog::new(&db).with_key_ring(KeyRing::single(7, event_hmac_key.to_vec()));
        log.commit_turn(
            &cid,
            EventSeq(0),
            vec![
                PendingEvent::encode(
                    EventKind::UserMsg,
                    &serde_json::json!({"text":"rotation drill"}),
                    None,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let backup = backup_path.to_string_lossy().replace('\'', "''");
        db.with_conn(|conn| {
            conn.execute_batch(&format!("VACUUM INTO '{backup}'"))?;
            Ok(())
        })
        .unwrap();
        db.rekey_sqlcipher(&new_key).unwrap();
        drop(db);

        let reopened = Database::open(&config(new_key, path.clone())).unwrap();
        reopened
            .with_conn(|conn| {
                let check: String =
                    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
                assert_eq!(check, "ok");
                let secret: Vec<u8> = conn.query_row(
                    "SELECT value_blob FROM vault_secrets WHERE name = 'drill-secret'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(secret, [0xCA, 0xFE]);
                let plugin_count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM state_plugins WHERE plugin_id = 'drill-plugin'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(plugin_count, 1);
                Ok(())
            })
            .unwrap();
        let rotated_log =
            EventLog::new(&reopened).with_key_ring(KeyRing::single(7, event_hmac_key.to_vec()));
        rotated_log
            .commit_turn(
                &cid,
                EventSeq(1),
                vec![
                    PendingEvent::encode(
                        EventKind::ModelTurn,
                        &serde_json::json!({"text":"after rotation"}),
                        None,
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        assert_eq!(
            rotated_log.replay_since(&cid, EventSeq(0)).unwrap().len(),
            2
        );
        drop(reopened);

        let backup = Database::open(&config(old_key, backup_path)).unwrap();
        assert_eq!(
            EventLog::new(&backup)
                .with_key_ring(KeyRing::single(7, event_hmac_key.to_vec()))
                .replay_since(&cid, EventSeq(0))
                .unwrap()
                .len(),
            1
        );
    }
}
