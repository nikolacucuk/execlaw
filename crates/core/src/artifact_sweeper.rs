//! Periodic expiration and orphan cleanup for content-addressed artifacts.

use crate::attachments::AttachmentStore;
use crate::db::Database;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{info, warn};

const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const ORPHAN_GRACE_SECONDS: i64 = 60 * 60;

/// Sweeps expired artifact references and crash-leftover files on configured roots.
#[derive(Clone)]
pub struct ArtifactSweeper {
    db: Database,
    roots: Arc<Vec<PathBuf>>,
}

impl ArtifactSweeper {
    /// Construct a sweeper. Roots should be dedicated content-addressed directories.
    pub fn new(db: Database, roots: Vec<PathBuf>) -> Self {
        Self {
            db,
            roots: Arc::new(roots),
        }
    }

    /// Sweep once at startup and hourly until the service stop signal arrives.
    pub async fn run(self, stop: Arc<Notify>) {
        info!("artifact sweeper running");
        loop {
            if let Err(error) = self.sweep_once().await {
                warn!(error = %error, "artifact sweep failed; will retry next interval");
            }
            tokio::select! {
                _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
                _ = stop.notified() => {
                    if let Err(error) = self.sweep_once().await {
                        warn!(error = %error, "final artifact sweep failed during shutdown");
                    }
                    break;
                }
            }
        }
    }

    /// Run expiration and orphan recovery over all configured roots.
    pub async fn sweep_once(&self) -> Result<(usize, usize), String> {
        let db = self.db.clone();
        let roots = self.roots.as_ref().clone();
        let now = chrono::Utc::now().timestamp();
        let executor = db.clone();
        executor
            .run_blocking(move || {
                let store = AttachmentStore::new(&db);
                let expired = store
                    .sweep_expired_plugin_artifacts(now)
                    .map_err(|error| error.to_string())?;
                let mut orphans = 0usize;
                for root in roots {
                    orphans = orphans.saturating_add(
                        store
                            .sweep_orphan_artifact_blobs(&root, now, ORPHAN_GRACE_SECONDS)
                            .map_err(|error| error.to_string())?,
                    );
                }
                Ok::<_, String>((expired, orphans))
            })
            .await
            .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConfig;
    use crate::migrations::MigrationRunner;

    #[tokio::test]
    async fn startup_sweep_runs_with_missing_roots() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let roots = vec![
            directory.path().join("absent"),
            directory.path().join("other"),
        ];
        let sweeper = ArtifactSweeper::new(db, roots);
        assert_eq!(sweeper.sweep_once().await.unwrap(), (0, 0));
    }
}
