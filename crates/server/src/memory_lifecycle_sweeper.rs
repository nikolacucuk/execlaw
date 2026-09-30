use execlaw_core::Database;
use execlaw_core::memory_lifecycle::PromotionStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{info, warn};

const INTERVAL: Duration = Duration::from_secs(60 * 60);
const MIN_HITS: u64 = 3;
const PROMOTION_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
const DEMOTION_IDLE_SECS: i64 = 30 * 24 * 60 * 60;
const BATCH_LIMIT: u32 = 64;

#[derive(Clone)]
pub struct MemoryLifecycleSweeper {
    db: Database,
}

impl MemoryLifecycleSweeper {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn run(&self, stop: Arc<Notify>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(INTERVAL) => {}
                _ = stop.notified() => {
                    self.sweep_once();
                    return;
                }
            }
            self.sweep_once();
        }
    }

    pub fn sweep_once(&self) {
        let now = chrono::Utc::now().timestamp();
        match PromotionStore::new(&self.db).sweep(
            now,
            MIN_HITS,
            now - PROMOTION_WINDOW_SECS,
            now - DEMOTION_IDLE_SECS,
            BATCH_LIMIT,
        ) {
            Ok(report) => info!(
                promotions = report.promotion_proposals,
                demotions = report.demotion_proposals,
                "memory lifecycle sweep completed"
            ),
            Err(error) => warn!(%error, "memory lifecycle sweep failed"),
        }
    }
}
