//! Controller-only scrubbed support diagnostics.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::get;
use execlaw_core::backends::{BackendPurpose, BackendStore};
use execlaw_core::db::Database;
use execlaw_core::users::{UserRole, UserStore};
use execlaw_core::{migrations::MigrationRunner, research::ResearchJobStore};
use serde::Serialize;
use std::path::Path;
use utoipa::ToSchema;

pub(crate) const STORAGE_WARNING_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const STORAGE_CRITICAL_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const STORAGE_MAX_WAL_BYTES: u64 = 1024 * 1024 * 1024;
const WAL_CHECKPOINT_WARNING_FRAMES: i64 = 1000;

#[derive(Debug, Serialize, ToSchema)]
pub struct SupportBundle {
    pub schema_version: u32,
    pub generated_at: i64,
    pub application: ApplicationDiagnostic,
    pub database: DatabaseDiagnostic,
    pub hardware: HardwareDiagnostic,
    pub protocols: ProtocolDiagnostic,
    pub authority: AuthorityDiagnostic,
    pub recovery: RecoveryDiagnostic,
    pub corrective_actions: Vec<CorrectiveAction>,
    /// The response is designed to omit prompts, message bodies, credentials,
    /// endpoint URLs, plugin names, and filesystem paths.
    pub content_policy: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApplicationDiagnostic {
    pub version: String,
    pub operating_system: String,
    pub architecture: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DatabaseDiagnostic {
    pub database_file_present: bool,
    pub encryption_mode: String,
    pub schema_migrations_applied: u32,
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
    pub journal_bytes: u64,
    pub referenced_blob_bytes: u64,
    pub available_disk_bytes: Option<u64>,
    pub storage_state: String,
    pub storage_action: Option<String>,
    pub wal_checkpoint_blocked: bool,
    pub wal_frames: i64,
    pub wal_frames_checkpointed: i64,
    pub database_queued_jobs: usize,
    pub database_running_jobs: usize,
    pub database_rejected_jobs: u64,
    pub database_completed_jobs: u64,
    pub database_queue_wait_micros_max: u64,
    pub database_service_micros_max: u64,
    pub transaction_micros_total: u64,
    pub transaction_micros_max: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GpuDiagnostic {
    pub vendor: String,
    pub model: Option<String>,
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HardwareDiagnostic {
    pub logical_cpu_count: Option<usize>,
    pub available_ram_mb: Option<u64>,
    pub total_detected_gpu_memory_mb: Option<u64>,
    pub gpus: Vec<GpuDiagnostic>,
    pub capacity_class: String,
    pub capacity_note: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BackendDiagnostic {
    pub purpose: String,
    pub configured: bool,
    pub mode: Option<String>,
    pub last_stage: Option<String>,
    pub has_successful_readiness: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProtocolDiagnostic {
    pub backends: Vec<BackendDiagnostic>,
    pub active_model_profiles: u64,
    pub invalidated_model_profiles: u64,
    pub qualified_capabilities: Vec<QualifiedCapabilityDiagnostic>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QualifiedCapabilityDiagnostic {
    pub capability: String,
    pub passing_profiles: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuthorityDiagnostic {
    pub installed_plugins: usize,
    pub enabled_plugins: usize,
    pub quarantined_plugins: usize,
    pub tool_rules: usize,
    pub enabled_tool_rules: usize,
    pub disabled_tool_rules: usize,
    pub removed_tool_rules: usize,
    pub wildcard_tool_rules: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatusCount {
    pub status: String,
    pub count: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecoveryDiagnostic {
    pub recoverable_runs_by_status: Vec<StatusCount>,
    pub active_research_jobs: i64,
    pub pending_research_deletions: usize,
    pub outbox_by_status: Vec<StatusCount>,
    pub approval_waits: u64,
    pub automation_runs_by_status: Vec<StatusCount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CorrectiveAction {
    pub code: String,
    pub action: String,
}

#[utoipa::path(
    get,
    path = "/api/admin/diagnostics/support-bundle",
    responses(
        (status = 200, description = "Scrubbed local support diagnostics", body = SupportBundle),
        (status = 403, description = "Controller role required")
    ),
    security(("bearer_jwt" = [])),
    tag = "diagnostics"
)]
pub async fn support_bundle(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<SupportBundle>, ApiError> {
    let role = UserStore::new(&state.db)
        .get_by_id(&user.user_id)
        .map_err(|_error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "diagnostics_auth_lookup_failed",
            message: "could not verify diagnostics access".into(),
        })?
        .map(|user| user.role);
    if role != Some(UserRole::Controller) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_required",
            message: "Controller role required".into(),
        });
    }
    let executor = state.db.clone();
    executor
        .run_blocking(move || build_support_bundle(&state))
        .await
        .map_err(ApiError::from)?
        .map(Json)
}

fn build_support_bundle(state: &AppState) -> Result<SupportBundle, ApiError> {
    let database_file_present = state.db.path().is_file();
    let schema_migrations_applied = MigrationRunner::new(&state.db)
        .applied_count()
        .map_err(|error| diagnostic_error("diagnostics_schema_read_failed", error))?;
    let file_sizes = state.db.file_sizes();
    let available_disk_bytes = available_space_for(state.db.path());
    let referenced_blob_bytes = referenced_blob_bytes(&state.db)?;
    let checkpoint = state
        .db
        .wal_checkpoint_status()
        .map_err(|error| diagnostic_error("diagnostics_wal_status_failed", error))?;
    let execution_metrics = state.db.execution_metrics();
    let (storage_state, storage_action) = storage_state(
        available_disk_bytes,
        file_sizes.wal_bytes,
        checkpoint.checkpoint_blocked,
        checkpoint.frames_in_wal,
        checkpoint.frames_checkpointed,
    );

    let hardware = execlaw_container_manager::detect();
    let available_ram_mb = execlaw_container_manager::available_ram_mb();
    let gpus = hardware
        .gpus
        .into_iter()
        .map(|gpu| GpuDiagnostic {
            vendor: format!("{:?}", gpu.vendor),
            model: gpu.model_name,
            memory_mb: gpu.memory_mb,
        })
        .collect::<Vec<_>>();
    let detected_gpu_memory = gpus.iter().filter_map(|gpu| gpu.memory_mb).sum::<u64>();
    let total_detected_gpu_memory_mb = gpus
        .iter()
        .all(|gpu| gpu.memory_mb.is_some())
        .then_some(detected_gpu_memory);
    let logical_cpu_count = std::thread::available_parallelism().ok().map(usize::from);
    let capacity_class = if detected_gpu_memory >= 24 * 1024
        || available_ram_mb.is_some_and(|ram| ram >= 64 * 1024)
    {
        "large_local_candidate"
    } else if detected_gpu_memory >= 8 * 1024
        || available_ram_mb.is_some_and(|ram| ram >= 32 * 1024)
    {
        "compact_or_quantized_local_candidate"
    } else if available_ram_mb.is_some() {
        "cpu_or_small_model_candidate"
    } else {
        "capacity_unknown"
    };

    let backend_store = BackendStore::new(&state.db);
    let mut backends = Vec::new();
    for purpose in BackendPurpose::all() {
        let backend = backend_store
            .get(*purpose)
            .map_err(|error| diagnostic_error("diagnostics_backend_read_failed", error))?;
        let readiness = backend_store
            .readiness(*purpose)
            .map_err(|error| diagnostic_error("diagnostics_readiness_read_failed", error))?;
        backends.push(BackendDiagnostic {
            purpose: purpose.as_str().to_owned(),
            configured: backend.as_ref().is_some_and(|row| {
                row.endpoint
                    .as_deref()
                    .is_some_and(|url| !url.trim().is_empty())
            }),
            mode: backend.map(|row| row.mode.as_str().to_owned()),
            last_stage: readiness.as_ref().map(|row| row.stage.clone()),
            has_successful_readiness: readiness
                .as_ref()
                .is_some_and(|row| row.last_success_at.is_some()),
        });
    }
    let (active_model_profiles, invalidated_model_profiles, observations) = state
        .db
        .with_conn(|connection| {
            let counts = connection.query_row(
                    "SELECT COALESCE(SUM(CASE WHEN invalidated_at IS NULL THEN 1 ELSE 0 END), 0), \
                            COALESCE(SUM(CASE WHEN invalidated_at IS NOT NULL THEN 1 ELSE 0 END), 0) \
                     FROM state_model_capability_profiles",
                    [],
                    |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
                )?;
            let mut statement = connection.prepare(
                "SELECT observed_json FROM state_model_capability_profiles WHERE invalidated_at IS NULL",
            )?;
            let observations = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((counts.0, counts.1, observations))
        })
        .map_err(|error| diagnostic_error("diagnostics_profile_read_failed", error))?;
    let qualified_capabilities = summarize_qualified_capabilities(&observations);

    let authority = state
        .db
        .with_conn(|connection| {
            let plugins: (usize, usize, usize) = connection.query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN enabled != 0 THEN 1 ELSE 0 END), 0), \
                        COALESCE(SUM(CASE WHEN health_status != 'healthy' THEN 1 ELSE 0 END), 0) \
                 FROM state_plugins",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let tools: (usize, usize, usize, usize) = connection.query_row(
                "SELECT COUNT(*), \
                        COALESCE(SUM(CASE WHEN enabled != 0 AND removed_at IS NULL THEN 1 ELSE 0 END), 0), \
                        COALESCE(SUM(CASE WHEN enabled = 0 THEN 1 ELSE 0 END), 0), \
                        COALESCE(SUM(CASE WHEN removed_at IS NOT NULL THEN 1 ELSE 0 END), 0) \
                 FROM config_tool_access",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            let mut statement = connection.prepare(
                "SELECT allowed_classes FROM config_tool_access \
                 WHERE enabled != 0 AND removed_at IS NULL",
            )?;
            let policy_rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let wildcard_count = policy_rows
                .iter()
                .filter(|serialized| {
                    serde_json::from_str::<Vec<String>>(serialized)
                        .is_ok_and(|classes| classes.iter().any(|class| class == "*"))
                })
                .count();
            Ok(AuthorityDiagnostic {
                installed_plugins: plugins.0,
                enabled_plugins: plugins.1,
                quarantined_plugins: plugins.2,
                tool_rules: tools.0,
                enabled_tool_rules: tools.1,
                disabled_tool_rules: tools.2,
                removed_tool_rules: tools.3,
                wildcard_tool_rules: wildcard_count,
            })
        })
        .map_err(|error| diagnostic_error("diagnostics_authority_read_failed", error))?;

    let recoverable_runs_by_status = query_status_counts(
        &state,
        "SELECT status, COUNT(*) FROM state_runs \
         WHERE status IN ('pending', 'running', 'waiting') GROUP BY status ORDER BY status",
        "diagnostics_run_recovery_read_failed",
    )?;
    let active_research_jobs = ResearchJobStore::new(&state.db)
        .active_count_global()
        .map_err(|error| diagnostic_error("diagnostics_research_read_failed", error))?;
    let (pending_research_deletions, outbox_by_status, automation_runs_by_status, approval_waits) = state
        .db
        .with_conn(|connection| {
            let pending_deletions = connection.query_row(
                "SELECT COUNT(*) FROM state_privacy_deletion_jobs WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )?;
            let mut outbox_statement = connection.prepare(
                "SELECT status, COUNT(*) FROM state_outbox GROUP BY status ORDER BY status",
            )?;
            let outbox = outbox_statement
                .query_map([], |row| {
                    Ok(StatusCount {
                        status: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut automation_statement = connection.prepare(
                "SELECT status, COUNT(*) FROM state_automation_runs GROUP BY status ORDER BY status",
            )?;
            let automations = automation_statement
                .query_map([], |row| {
                    Ok(StatusCount {
                        status: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let approval_waits = connection.query_row(
                "SELECT (SELECT COUNT(*) FROM state_run_steps WHERE kind = 'approval_wait' AND status = 'waiting') \
                      + (SELECT COUNT(*) FROM state_chain_runs WHERE status = 'awaiting_approval')",
                [],
                |row| row.get(0),
            )?;
            Ok((pending_deletions, outbox, automations, approval_waits))
        })
        .map_err(|error| diagnostic_error("diagnostics_recovery_read_failed", error))?;

    let hardware = HardwareDiagnostic {
        logical_cpu_count,
        available_ram_mb,
        total_detected_gpu_memory_mb,
        gpus,
        capacity_class: capacity_class.to_owned(),
        capacity_note: "Capacity heuristic only; qualify the exact local model and backend before routing production turns.".into(),
    };
    let database = DatabaseDiagnostic {
        database_file_present,
        encryption_mode: if state.db_config.key.is_some() {
            "sqlcipher_keyed_connection".into()
        } else {
            "plaintext_sqlite_connection".into()
        },
        schema_migrations_applied,
        database_bytes: file_sizes.database_bytes,
        wal_bytes: file_sizes.wal_bytes,
        shm_bytes: file_sizes.shm_bytes,
        journal_bytes: file_sizes.journal_bytes,
        referenced_blob_bytes,
        available_disk_bytes,
        storage_state: storage_state.to_owned(),
        storage_action: storage_action.map(str::to_owned),
        wal_checkpoint_blocked: checkpoint.checkpoint_blocked,
        wal_frames: checkpoint.frames_in_wal,
        wal_frames_checkpointed: checkpoint.frames_checkpointed,
        database_queued_jobs: execution_metrics.queued_jobs,
        database_running_jobs: execution_metrics.running_jobs,
        database_rejected_jobs: execution_metrics.rejected_jobs,
        database_completed_jobs: execution_metrics.completed_jobs,
        database_queue_wait_micros_max: execution_metrics.queue_wait_micros_max,
        database_service_micros_max: execution_metrics.service_micros_max,
        transaction_micros_total: execution_metrics.transaction_micros_total,
        transaction_micros_max: execution_metrics.transaction_micros_max,
    };

    let mut corrective_actions = Vec::new();
    if database.encryption_mode == "plaintext_sqlite_connection" {
        corrective_actions.push(CorrectiveAction {
            code: "database_not_encrypted".into(),
            action: "For production state, use the SQLCipher-enabled release and run the backup/rotation qualification before migrating.".into(),
        });
    }
    if let Some(action) = &database.storage_action {
        corrective_actions.push(CorrectiveAction {
            code: format!("storage_{}", database.storage_state),
            action: action.clone(),
        });
    }
    if backends.iter().all(|backend| !backend.configured) {
        corrective_actions.push(CorrectiveAction {
            code: "local_backend_unconfigured".into(),
            action: "Configure a local inference backend in Settings > Backends.".into(),
        });
    } else if active_model_profiles == 0 {
        corrective_actions.push(CorrectiveAction {
            code: "model_protocol_unqualified".into(),
            action: "Run protocol and capability qualification for each deployed model identity before enabling routing.".into(),
        });
    }
    if authority.quarantined_plugins > 0 {
        corrective_actions.push(CorrectiveAction {
            code: "plugins_quarantined".into(),
            action: "Review quarantined plugins in Settings > Plugins and repair or reinstall the affected plugin.".into(),
        });
    }
    if !recoverable_runs_by_status.is_empty() {
        corrective_actions.push(CorrectiveAction {
            code: "runs_need_recovery".into(),
            action: "Open Settings > Runs and inspect, resume, cancel, or reconcile the listed recoverable runs.".into(),
        });
    }
    if outbox_by_status
        .iter()
        .any(|row| row.status == "dead_letter" || row.status == "failed")
    {
        corrective_actions.push(CorrectiveAction {
            code: "outbox_needs_review".into(),
            action: "Review pending and dead-letter deliveries before redriving effects.".into(),
        });
    }
    if pending_research_deletions > 0 {
        corrective_actions.push(CorrectiveAction {
            code: "research_deletions_pending".into(),
            action: "Inspect the Research page deletion status; retry the configured local projection storage if cleanup remains pending.".into(),
        });
    }

    Ok(SupportBundle {
        schema_version: 2,
        generated_at: chrono::Utc::now().timestamp(),
        application: ApplicationDiagnostic {
            version: env!("CARGO_PKG_VERSION").into(),
            operating_system: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
        },
        database,
        hardware,
        protocols: ProtocolDiagnostic {
            backends,
            active_model_profiles,
            invalidated_model_profiles,
            qualified_capabilities,
        },
        authority,
        recovery: RecoveryDiagnostic {
            recoverable_runs_by_status,
            active_research_jobs,
            pending_research_deletions,
            outbox_by_status,
            approval_waits,
            automation_runs_by_status,
        },
        corrective_actions,
        content_policy: "Counts, statuses, capacities, and corrective actions only; no prompts, message bodies, credentials, endpoints, paths, plugin names, or raw errors.".into(),
    })
}

fn summarize_qualified_capabilities(observations: &[String]) -> Vec<QualifiedCapabilityDiagnostic> {
    const CAPABILITIES: &[&str] = &[
        "text",
        "streaming",
        "tools",
        "structured_json",
        "context",
        "vision",
    ];
    CAPABILITIES
        .iter()
        .map(|capability| QualifiedCapabilityDiagnostic {
            capability: (*capability).to_owned(),
            passing_profiles: observations
                .iter()
                .filter(|serialized| {
                    serde_json::from_str::<serde_json::Value>(serialized)
                        .ok()
                        .and_then(|observed| observed.get(*capability)?.get("passed")?.as_bool())
                        == Some(true)
                })
                .count() as u64,
        })
        .collect()
}

pub(crate) fn optional_work_denial(database: &Database, write_path: &Path) -> Option<&'static str> {
    if database.path() == Path::new(":memory:") {
        return None;
    }
    let Some(database_free) = available_space_for(database.path()) else {
        return Some("optional work paused because database free space cannot be measured");
    };
    let Some(target_free) = available_space_for(write_path) else {
        return Some("optional work paused because target free space cannot be measured");
    };
    optional_work_denial_for(database_free, target_free, database.file_sizes().wal_bytes)
}

fn optional_work_denial_for(
    database_free: u64,
    target_free: u64,
    wal_bytes: u64,
) -> Option<&'static str> {
    if database_free < STORAGE_WARNING_RESERVE_BYTES || target_free < STORAGE_WARNING_RESERVE_BYTES
    {
        return Some("optional work paused to preserve the local storage reserve");
    }
    if wal_bytes > STORAGE_MAX_WAL_BYTES {
        return Some("optional work paused while WAL growth is investigated");
    }
    None
}

fn available_space_for(path: &Path) -> Option<u64> {
    if path == Path::new(":memory:") {
        return None;
    }
    let candidate = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let existing =
        std::iter::successors(Some(candidate), |path| path.parent()).find(|path| path.is_dir())?;
    fs4::available_space(existing).ok()
}

fn referenced_blob_bytes(database: &Database) -> Result<u64, ApiError> {
    let paths = database
        .with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT path FROM state_attachments UNION ALL SELECT path FROM state_artifacts",
            )?;
            Ok(statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .map_err(|error| diagnostic_error("diagnostics_blob_size_failed", error))?;
    Ok(paths.into_iter().fold(0u64, |total, path| {
        let size = std::fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| metadata.file_type().is_file())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        total.saturating_add(size)
    }))
}

fn storage_state(
    available_bytes: Option<u64>,
    wal_bytes: u64,
    checkpoint_blocked: bool,
    wal_frames: i64,
    checkpointed_frames: i64,
) -> (&'static str, Option<&'static str>) {
    let Some(available_bytes) = available_bytes else {
        return (
            "unknown",
            Some(
                "Free space could not be measured; optional work is paused until it can be checked.",
            ),
        );
    };
    if available_bytes < STORAGE_CRITICAL_RESERVE_BYTES || wal_bytes > STORAGE_MAX_WAL_BYTES {
        return (
            "critical",
            Some(
                "Pause optional indexing and model downloads; free disk space, then retry maintenance.",
            ),
        );
    }
    let uncheckpointed_frames = wal_frames.saturating_sub(checkpointed_frames);
    if available_bytes < STORAGE_WARNING_RESERVE_BYTES
        || checkpoint_blocked
        || uncheckpointed_frames >= WAL_CHECKPOINT_WARNING_FRAMES
    {
        return (
            "warning",
            Some(
                "Review the disk reserve and WAL backlog; long-running readers can stall checkpoint progress.",
            ),
        );
    }
    ("healthy", None)
}

fn query_status_counts(
    state: &AppState,
    sql: &'static str,
    error_code: &'static str,
) -> Result<Vec<StatusCount>, ApiError> {
    state
        .db
        .with_conn(|connection| {
            let mut statement = connection.prepare(sql)?;
            statement
                .query_map([], |row| {
                    Ok(StatusCount {
                        status: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(execlaw_core::db::DbError::from)
        })
        .map_err(|error| diagnostic_error(error_code, error))
}

fn diagnostic_error(code: &'static str, error: impl std::fmt::Display) -> ApiError {
    tracing::warn!(code, error = %error, "support-bundle snapshot failed");
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code,
        message: "could not collect scrubbed diagnostics".into(),
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/admin/diagnostics/support-bundle", get(support_bundle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_summary_contains_only_pass_counts() {
        let observations = vec![
            serde_json::json!({
                "text": {"passed": true},
                "streaming": {"passed": true},
                "tools": {"passed": false},
                "structured_json": {"passed": true},
                "context": {"passed": true},
                "vision": {"passed": false},
                "private_detail": "must not appear",
            })
            .to_string(),
            serde_json::json!({
                "text": {"passed": true},
                "streaming": {"passed": true},
                "tools": {"passed": true},
                "structured_json": {"passed": true},
                "context": {"passed": true},
                "vision": {"passed": false},
            })
            .to_string(),
        ];

        let summary = summarize_qualified_capabilities(&observations);
        let text = summary
            .iter()
            .find(|item| item.capability == "text")
            .unwrap();
        let tools = summary
            .iter()
            .find(|item| item.capability == "tools")
            .unwrap();
        let vision = summary
            .iter()
            .find(|item| item.capability == "vision")
            .unwrap();
        assert_eq!(text.passing_profiles, 2);
        assert_eq!(tools.passing_profiles, 1);
        assert_eq!(vision.passing_profiles, 0);
        let serialized = serde_json::to_string(&summary).unwrap();
        assert!(!serialized.contains("private_detail"));
    }

    #[test]
    fn support_bundle_omits_raw_model_profile_details() {
        let state = crate::routes::test_app_state();
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_model_capability_profiles \
                     (identity_hash, model_id, quantization, chat_template, backend_version, \
                      parser_version, context_tokens, observed_json, qualified_at) \
                     VALUES ('profile-id-secret', 'private-model-name', 'private-quant', \
                             'private-template', 'private-backend-version', 'private-parser', 4096, \
                             ?1, 1)",
                    [serde_json::json!({
                        "text": {"passed": true},
                        "private_prompt": "PROMPT_SENTINEL",
                        "access_token": "TOKEN_SENTINEL",
                    })
                    .to_string()],
                )?;
                Ok(())
            })
            .unwrap();

        let serialized = serde_json::to_string(&build_support_bundle(&state).unwrap()).unwrap();
        assert!(serialized.contains("\"passing_profiles\":1"));
        for private_value in [
            "profile-id-secret",
            "private-model-name",
            "private-quant",
            "private-template",
            "private-backend-version",
            "private-parser",
            "PROMPT_SENTINEL",
            "TOKEN_SENTINEL",
        ] {
            assert!(
                !serialized.contains(private_value),
                "support bundle leaked {private_value}"
            );
        }
    }

    #[test]
    fn storage_health_distinguishes_reserve_wal_and_checkpoint_pressure() {
        assert_eq!(
            storage_state(Some(STORAGE_CRITICAL_RESERVE_BYTES - 1), 0, false, 0, 0,).0,
            "critical"
        );
        assert_eq!(
            storage_state(
                Some(STORAGE_WARNING_RESERVE_BYTES),
                STORAGE_MAX_WAL_BYTES + 1,
                false,
                0,
                0,
            )
            .0,
            "critical"
        );
        assert_eq!(
            storage_state(Some(STORAGE_WARNING_RESERVE_BYTES - 1), 0, false, 0, 0,).0,
            "warning"
        );
        assert_eq!(
            storage_state(Some(STORAGE_WARNING_RESERVE_BYTES), 0, true, 10, 4).0,
            "warning"
        );
        assert_eq!(
            storage_state(Some(STORAGE_WARNING_RESERVE_BYTES), 0, false, 5000, 3999).0,
            "warning"
        );
        assert_eq!(
            storage_state(Some(STORAGE_WARNING_RESERVE_BYTES), 0, false, 10, 10).0,
            "healthy"
        );
        assert!(
            optional_work_denial_for(STORAGE_WARNING_RESERVE_BYTES - 1, u64::MAX, 0,).is_some()
        );
        assert!(optional_work_denial_for(u64::MAX, u64::MAX, STORAGE_MAX_WAL_BYTES + 1,).is_some());
        assert!(optional_work_denial_for(u64::MAX, u64::MAX, STORAGE_MAX_WAL_BYTES).is_none());
    }
}
