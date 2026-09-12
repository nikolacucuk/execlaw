//! Durable Graphiti ingestion and reconciliation worker.

use execlaw_core::Database;
use execlaw_core::graphiti::GraphitiJobStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const LEASE_SECS: i64 = 60;
const MAX_BACKOFF_SECS: i64 = 300;

pub struct GraphitiWorker {
    db: Database,
    worker_id: String,
}

impl GraphitiWorker {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            worker_id: format!("graphiti-{}", uuid::Uuid::new_v4()),
        }
    }

    pub async fn run(self, stop: Arc<Notify>) {
        loop {
            if let Err(error) = self.process_one().await {
                tracing::warn!(worker_id = %self.worker_id, error = %error, "Graphiti worker iteration failed");
            }
            tokio::select! {
                _ = stop.notified() => break,
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    async fn process_one(&self) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp();
        let store = GraphitiJobStore::new(&self.db);
        let Some(job) = store
            .claim_next(&self.worker_id, now, now + LEASE_SECS)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };

        match crate::graphiti_tool::execute_job(&self.db, &job).await {
            Ok(()) => {
                store
                    .complete(&job.job_id, &self.worker_id, chrono::Utc::now().timestamp())
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => {
                let backoff = 2_i64
                    .saturating_pow(job.attempt.saturating_sub(1) as u32)
                    .min(MAX_BACKOFF_SECS);
                store
                    .retry(
                        &job.job_id,
                        &self.worker_id,
                        &error,
                        chrono::Utc::now().timestamp() + backoff,
                    )
                    .map_err(|store_error| store_error.to_string())?;
                tracing::warn!(
                    job_id = %job.job_id,
                    evidence_id = %job.evidence_id,
                    attempt = job.attempt,
                    error = %error,
                    "Graphiti job scheduled for retry"
                );
            }
        }
        Ok(())
    }
}
