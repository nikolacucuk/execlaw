//! Root-scoped workspace inspection and immutable checkpoint materialization.

use crate::{auth_extract::AuthedUser, routes::ApiError, state::AppState};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
};
use execlaw_core::{
    users::UserRole,
    workspaces::{
        WorkspaceApplyReceipt, WorkspaceCheckpointRecord, WorkspaceRootRecord, WorkspaceStore,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path as FsPath, PathBuf};

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 500;

#[derive(Debug, Deserialize)]
pub struct RegisterWorkspaceRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckpointRequest {
    pub workspace_id: String,
    pub parent_checkpoint_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApplyWorkspaceDiffRequest {
    pub preview_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct RestoreWorkspaceRequest {
    pub apply_id: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspacePathQuery {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceSearchRequest {
    pub query: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceCheckpointView {
    pub checkpoint_id: String,
    pub workspace_id: String,
    pub run_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub file_count: usize,
    pub total_bytes: u64,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceSearchMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceDiffChange {
    pub path: String,
    pub base_sha256: Option<String>,
    pub current_sha256: Option<String>,
    pub proposed_sha256: Option<String>,
    pub conflict: bool,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceDiffPreview {
    pub run_id: String,
    pub workspace_id: String,
    pub checkpoint_id: String,
    pub preview_hash: String,
    pub changes: Vec<WorkspaceDiffChange>,
    pub conflicts: usize,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceApplyState {
    pub latest_apply: Option<WorkspaceApplyReceipt>,
    pub latest_restore: Option<WorkspaceApplyReceipt>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/workspaces", get(list_roots).post(register_root))
        .route("/api/admin/workspaces/{workspace_id}/files", get(read_file))
        .route(
            "/api/admin/workspaces/{workspace_id}/search",
            post(search_files),
        )
        .route(
            "/api/admin/runs/{run_id}/workspace-checkpoints",
            post(create_checkpoint),
        )
        .route(
            "/api/admin/runs/{run_id}/workspace-diff",
            get(preview_run_diff),
        )
        .route(
            "/api/admin/runs/{run_id}/workspace-apply",
            get(get_workspace_apply_state).post(apply_run_diff),
        )
        .route(
            "/api/admin/runs/{run_id}/workspace-restore",
            post(restore_run_workspace),
        )
}

#[utoipa::path(
    get,
    path = "/api/admin/runs/{run_id}/workspace-apply",
    params(("run_id" = String, Path, description = "Run workspace apply state")),
    responses((status = 200, description = "Latest apply and restore receipts", body = serde_json::Value)),
    tag = "workspaces"
)]
pub async fn get_workspace_apply_state(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
) -> Result<Json<WorkspaceApplyState>, ApiError> {
    controller(&user)?;
    let store = WorkspaceStore::new(&state.db);
    Ok(Json(WorkspaceApplyState {
        latest_apply: store
            .latest_applied_workspace_diff(&run_id)
            .map_err(map_store)?,
        latest_restore: store.latest_workspace_restore(&run_id).map_err(map_store)?,
    }))
}

fn controller(user: &AuthedUser) -> Result<(), ApiError> {
    if user.role == UserRole::Controller {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_required",
            message: "Controller role required".into(),
        })
    }
}

fn map_store(error: execlaw_core::workspaces::WorkspaceStoreError) -> ApiError {
    let invalid = matches!(
        &error,
        execlaw_core::workspaces::WorkspaceStoreError::Invalid(_)
    );
    ApiError {
        status: if invalid {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        },
        code: "workspace_error",
        message: error.to_string(),
    }
}

#[utoipa::path(post,path="/api/admin/workspaces",request_body=serde_json::Value,responses((status=200,description="Registered canonical workspace root",body=serde_json::Value)),tag="workspaces")]
pub async fn register_root(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<RegisterWorkspaceRequest>,
) -> Result<Json<WorkspaceRootRecord>, ApiError> {
    controller(&user)?;
    let input = PathBuf::from(&request.path);
    let canonical = std::fs::canonicalize(&input).map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "workspace_root_invalid",
        message: error.to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_root_invalid",
            message: "workspace root must be an existing directory".into(),
        });
    }
    let text = canonical.to_str().ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "workspace_root_invalid",
        message: "workspace root path is not valid Unicode".into(),
    })?;
    WorkspaceStore::new(&state.db)
        .register_root(text, &user.user_id, chrono::Utc::now().timestamp())
        .map(Json)
        .map_err(map_store)
}

#[utoipa::path(get,path="/api/admin/workspaces",responses((status=200,description="Registered workspace roots",body=[serde_json::Value])),tag="workspaces")]
pub async fn list_roots(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<Vec<WorkspaceRootRecord>>, ApiError> {
    controller(&user)?;
    WorkspaceStore::new(&state.db)
        .list_roots()
        .map(Json)
        .map_err(map_store)
}

#[utoipa::path(get,path="/api/admin/workspaces/{workspace_id}/files",params(("workspace_id"=String,Path),("path"=String,Query)),responses((status=200,description="Root-scoped UTF-8 workspace file",body=serde_json::Value)),tag="workspaces")]
pub async fn read_file(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(workspace_id): Path<String>,
    Query(query): Query<WorkspacePathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    let root = root_path(&state, &workspace_id)?;
    let path = safe_relative_file(&root, &query.path)?;
    let metadata =
        std::fs::metadata(&path).map_err(|error| io_error("workspace_file_read", error))?;
    if has_multiple_links(&metadata) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_hardlink_denied",
            message: "files with multiple hard links are not readable through workspace tools"
                .into(),
        });
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "workspace_file_too_large",
            message: "file exceeds 10 MiB read limit".into(),
        });
    }
    let bytes = std::fs::read(&path).map_err(|error| io_error("workspace_file_read", error))?;
    let text = String::from_utf8(bytes).map_err(|_| ApiError {
        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
        code: "workspace_file_binary",
        message: "workspace read_file only returns UTF-8 text".into(),
    })?;
    Ok(Json(serde_json::json!({"path":query.path,"text":text})))
}

#[utoipa::path(post,path="/api/admin/workspaces/{workspace_id}/search",params(("workspace_id"=String,Path)),request_body=serde_json::Value,responses((status=200,description="Bounded workspace search results",body=[serde_json::Value])),tag="workspaces")]
pub async fn search_files(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(workspace_id): Path<String>,
    Json(request): Json<WorkspaceSearchRequest>,
) -> Result<Json<Vec<WorkspaceSearchMatch>>, ApiError> {
    controller(&user)?;
    if request.query.is_empty() || request.query.len() > 1024 {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_search_query_invalid",
            message: "search query must contain 1 to 1024 bytes".into(),
        });
    }
    let root = root_path(&state, &workspace_id)?;
    let files = scan_workspace(&root)?;
    let mut matches = Vec::new();
    for (path, bytes) in files {
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if line.contains(&request.query) {
                matches.push(WorkspaceSearchMatch {
                    path: path.clone(),
                    line: index as u64 + 1,
                    text: line.chars().take(2048).collect(),
                });
                if matches.len() >= MAX_SEARCH_MATCHES {
                    return Ok(Json(matches));
                }
            }
        }
    }
    Ok(Json(matches))
}

#[utoipa::path(post,path="/api/admin/runs/{run_id}/workspace-checkpoints",params(("run_id"=String,Path)),request_body=serde_json::Value,responses((status=201,description="Content-addressed workspace checkpoint and isolated checkout",body=serde_json::Value)),tag="workspaces")]
pub async fn create_checkpoint(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    Json(request): Json<CheckpointRequest>,
) -> Result<(StatusCode, Json<WorkspaceCheckpointView>), ApiError> {
    controller(&user)?;
    let store = WorkspaceStore::new(&state.db);
    let run = execlaw_core::runs::RunStore::new(&state.db)
        .get_run(&run_id)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "workspace_run_lookup_failed",
            message: error.to_string(),
        })?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "run_not_found",
            message: "durable run not found".into(),
        })?;
    let root = store
        .get_root(&request.workspace_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_not_found",
            message: "registered workspace root not found".into(),
        })?;
    if let Some(parent_id) = request.parent_checkpoint_id.as_deref() {
        let parent = store
            .get_checkpoint(parent_id)
            .map_err(map_store)?
            .ok_or_else(|| ApiError {
                status: StatusCode::NOT_FOUND,
                code: "workspace_checkpoint_not_found",
                message: "parent checkpoint not found".into(),
            })?;
        if parent.workspace_id != request.workspace_id {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "workspace_checkpoint_mismatch",
                message: "parent checkpoint belongs to another workspace".into(),
            });
        }
    }
    let files = scan_workspace(FsPath::new(&root.canonical_path))?;
    let now = chrono::Utc::now().timestamp();
    let checkpoint = store
        .create_checkpoint(
            &request.workspace_id,
            &run.run_id,
            request.parent_checkpoint_id.as_deref(),
            &files,
            now,
        )
        .map_err(map_store)?;
    let checkout = state
        .data_dir
        .join("workspace-checkouts")
        .join(hex::encode(Sha256::digest(
            format!("{}:{}", run.run_id, checkpoint.checkpoint_id).as_bytes(),
        )));
    materialize_checkpoint(&store, &checkpoint, &checkout)?;
    store
        .bind_run_checkout(
            &run.run_id,
            &request.workspace_id,
            &checkpoint.checkpoint_id,
            &checkout.to_string_lossy(),
            now,
        )
        .map_err(map_store)?;
    Ok((
        StatusCode::CREATED,
        Json(WorkspaceCheckpointView {
            checkpoint_id: checkpoint.checkpoint_id,
            workspace_id: checkpoint.workspace_id,
            run_id: checkpoint.run_id,
            parent_checkpoint_id: checkpoint.parent_checkpoint_id,
            file_count: checkpoint.manifest.len(),
            total_bytes: checkpoint.total_bytes,
            created_at: checkpoint.created_at,
        }),
    ))
}

#[utoipa::path(get,path="/api/admin/runs/{run_id}/workspace-diff",params(("run_id"=String,Path)),responses((status=200,description="Conflict-aware workspace diff preview",body=serde_json::Value)),tag="workspaces")]
pub async fn preview_run_diff(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
) -> Result<Json<WorkspaceDiffPreview>, ApiError> {
    controller(&user)?;
    compute_workspace_diff(&state, &run_id).map(Json)
}

#[utoipa::path(
    post,
    path = "/api/admin/runs/{run_id}/workspace-apply",
    params(("run_id" = String, Path, description = "Run whose isolated checkout will be applied"), ("Idempotency-Key" = String, Header, description = "Idempotency key for this reviewed diff")),
    request_body = serde_json::Value,
    responses((status = 200, description = "Per-file durable apply receipt", body = serde_json::Value)),
    tag = "workspaces"
)]
pub async fn apply_run_diff(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ApplyWorkspaceDiffRequest>,
) -> Result<Json<execlaw_core::workspaces::WorkspaceApplyReceipt>, ApiError> {
    controller(&user)?;
    let request_id = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        })
        .ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_apply_idempotency_required",
            message: "a visible-ASCII Idempotency-Key is required".into(),
        })?;
    let store = WorkspaceStore::new(&state.db);
    let existing = store
        .get_apply_by_request(&run_id, request_id)
        .map_err(map_store)?;
    let (preview, receipt) = if let Some(receipt) = existing {
        if receipt.preview_hash != request.preview_hash {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_apply_intent_conflict",
                message: "idempotency key was already bound to another diff preview".into(),
            });
        }
        if receipt.status == "applied" {
            return Ok(Json(receipt));
        }
        if receipt.status == "conflict" {
            return Err(ApiError {status:StatusCode::CONFLICT,code:"workspace_apply_conflict",message:"the previous apply stopped on a concurrent edit; create a fresh checkpoint and preview".into()});
        }
        (None, receipt)
    } else {
        let preview = compute_workspace_diff(&state, &run_id)?;
        if preview.preview_hash != request.preview_hash {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_preview_stale",
                message:
                    "workspace changed after preview; refresh the conflict preview before applying"
                        .into(),
            });
        }
        if preview.conflicts > 0 {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_diff_conflict",
                message: "diff preview contains external edits; no files were changed".into(),
            });
        }
        let files = preview
            .changes
            .iter()
            .map(|change| execlaw_core::workspaces::WorkspaceApplyFile {
                path: change.path.clone(),
                base_sha256: change.base_sha256.clone(),
                proposed_sha256: change.proposed_sha256.clone(),
                status: "pending".into(),
                error: None,
            })
            .collect::<Vec<_>>();
        let (receipt, _created) = store
            .begin_apply(
                &run_id,
                &preview.workspace_id,
                &preview.checkpoint_id,
                &preview.preview_hash,
                request_id,
                &files,
                chrono::Utc::now().timestamp(),
            )
            .map_err(map_store)?;
        (Some(preview), receipt)
    };
    let owner = uuid::Uuid::new_v4().to_string();
    if !store
        .claim_apply(
            &receipt.apply_id,
            &owner,
            chrono::Utc::now().timestamp(),
            60,
        )
        .map_err(map_store)?
    {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_apply_busy",
            message: "another request is applying this workspace diff".into(),
        });
    }
    let binding = store
        .binding_for_run(&run_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_binding_not_found",
            message: "run has no workspace checkout".into(),
        })?;
    let root = root_path(&state, &receipt.workspace_id)?;
    let checkout = std::fs::canonicalize(&binding.checkout_path)
        .map_err(|error| io_error("workspace_checkout_read", error))?;
    execute_apply_files(&store, &receipt, &owner, &root, &checkout)?;
    let _ = preview;
    store
        .get_apply_by_request(&run_id, request_id)
        .map_err(map_store)?
        .map(Json)
        .ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "workspace_apply_receipt_missing",
            message: "apply receipt disappeared after completion".into(),
        })
}

#[utoipa::path(
    post,
    path = "/api/admin/runs/{run_id}/workspace-restore",
    params(("run_id" = String, Path, description = "Run whose owned workspace changes should be restored"), ("Idempotency-Key" = String, Header, description = "Idempotency key for this restore")),
    request_body = serde_json::Value,
    responses((status = 200, description = "Per-file durable restore receipt", body = serde_json::Value)),
    tag = "workspaces"
)]
pub async fn restore_run_workspace(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RestoreWorkspaceRequest>,
) -> Result<Json<execlaw_core::workspaces::WorkspaceApplyReceipt>, ApiError> {
    controller(&user)?;
    let request_id = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        })
        .ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_restore_idempotency_required",
            message: "a visible-ASCII Idempotency-Key is required".into(),
        })?;
    let store = WorkspaceStore::new(&state.db);
    let source = store
        .latest_applied_workspace_diff(&run_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_apply_not_found",
            message: "run has no successfully applied workspace diff to restore".into(),
        })?;
    if source.apply_id != request.apply_id {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_restore_source_stale",
            message: "only the latest successfully applied diff can be restored".into(),
        });
    }
    let internal_request_id = format!(
        "restore:{}:{}",
        source.apply_id,
        hex::encode(Sha256::digest(request_id.as_bytes()))
    );
    let existing = store
        .get_apply_by_request(&run_id, &internal_request_id)
        .map_err(map_store)?;
    if let Some(receipt) = existing.as_ref() {
        if receipt.status == "applied" {
            return Ok(Json(receipt.clone()));
        }
        if receipt.status == "conflict" {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_restore_conflict",
                message: "previous restore found later edits; create a fresh checkpoint before another restore".into(),
            });
        }
    }

    let root = root_path(&state, &source.workspace_id)?;
    let restore_files = if let Some(receipt) = existing.as_ref() {
        // Reuse the durable per-file plan after a process restart. The file
        // applier recognizes already-restored bytes by their proposed hash.
        store
            .list_apply_files(&receipt.apply_id)
            .map_err(map_store)?
    } else {
        let source_files = store
            .list_apply_files(&source.apply_id)
            .map_err(map_store)?;
        let current = hash_files(scan_workspace(&root)?);
        let mut restore_files = Vec::new();
        for file in source_files.iter().filter(|file| file.status == "applied") {
            if current.get(&file.path).cloned() != file.proposed_sha256 {
                return Err(ApiError {
                    status: StatusCode::CONFLICT,
                    code: "workspace_restore_conflict",
                    message: format!(
                        "'{}' changed after this run applied it; restore refuses to overwrite that edit",
                        file.path
                    ),
                });
            }
            restore_files.push(execlaw_core::workspaces::WorkspaceApplyFile {
                path: file.path.clone(),
                base_sha256: file.proposed_sha256.clone(),
                proposed_sha256: file.base_sha256.clone(),
                status: "pending".into(),
                error: None,
            });
        }
        restore_files
    };
    let restore_hash = hex::encode(Sha256::digest(
        serde_json::to_vec(&serde_json::json!({
            "restore_of": source.apply_id,
            "files": restore_files
        }))
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "workspace_restore_encode",
            message: error.to_string(),
        })?,
    ));
    let receipt = if let Some(receipt) = existing {
        receipt
    } else {
        store
            .begin_apply(
                &run_id,
                &source.workspace_id,
                &source.checkpoint_id,
                &restore_hash,
                &internal_request_id,
                &restore_files,
                chrono::Utc::now().timestamp(),
            )
            .map_err(map_store)?
            .0
    };
    let owner = uuid::Uuid::new_v4().to_string();
    if !store
        .claim_apply(
            &receipt.apply_id,
            &owner,
            chrono::Utc::now().timestamp(),
            60,
        )
        .map_err(map_store)?
    {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_apply_busy",
            message: "another apply or restore is using this workspace".into(),
        });
    }
    let checkpoint = store
        .get_checkpoint(&source.checkpoint_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_checkpoint_missing",
            message: "the run's base checkpoint is missing".into(),
        })?;
    let checkout_parent = state.data_dir.join("workspace-checkouts");
    std::fs::create_dir_all(&checkout_parent)
        .map_err(|error| io_error("workspace_restore_checkout_root", error))?;
    let restore_checkout_temp = tempfile::Builder::new()
        .prefix("restore-")
        .tempdir_in(&checkout_parent)
        .map_err(|error| io_error("workspace_restore_checkout_create", error))?;
    materialize_checkpoint(&store, &checkpoint, restore_checkout_temp.path())?;
    let restore_checkout = std::fs::canonicalize(restore_checkout_temp.path())
        .map_err(|error| io_error("workspace_restore_checkout", error))?;
    let checkout_root = std::fs::canonicalize(&checkout_parent)
        .map_err(|error| io_error("workspace_restore_checkout_root", error))?;
    if !restore_checkout.starts_with(&checkout_root) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_checkout_escape",
            message: "restore checkout resolves outside the managed workspace directory".into(),
        });
    }
    execute_apply_files(&store, &receipt, &owner, &root, &restore_checkout)?;
    store
        .get_apply_by_request(&run_id, &internal_request_id)
        .map_err(map_store)?
        .map(Json)
        .ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "workspace_restore_receipt_missing",
            message: "restore receipt disappeared after completion".into(),
        })
}

enum WorkspaceApplyError {
    Conflict(String),
    Failure(String),
}

fn execute_apply_files(
    store: &WorkspaceStore<'_>,
    receipt: &execlaw_core::workspaces::WorkspaceApplyReceipt,
    owner: &str,
    root: &FsPath,
    checkout: &FsPath,
) -> Result<(), ApiError> {
    for file in store
        .list_apply_files(&receipt.apply_id)
        .map_err(map_store)?
    {
        if file.status == "applied" {
            continue;
        }
        if file.status == "conflict" {
            store
                .finish_apply(
                    &receipt.apply_id,
                    owner,
                    "conflict",
                    chrono::Utc::now().timestamp(),
                )
                .map_err(map_store)?;
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_apply_conflict",
                message: "a previous attempt found a concurrent edit".into(),
            });
        }
        match apply_workspace_file(root, checkout, &receipt.apply_id, &file) {
            Ok(()) => store
                .update_apply_file(
                    &receipt.apply_id,
                    owner,
                    &file.path,
                    "applied",
                    None,
                    "applying",
                    chrono::Utc::now().timestamp(),
                )
                .map_err(map_store)?,
            Err(WorkspaceApplyError::Conflict(detail)) => {
                store
                    .update_apply_file(
                        &receipt.apply_id,
                        owner,
                        &file.path,
                        "conflict",
                        Some(&detail),
                        "applying",
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(map_store)?;
                store
                    .finish_apply(
                        &receipt.apply_id,
                        owner,
                        "conflict",
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(map_store)?;
                return Err(ApiError {
                    status: StatusCode::CONFLICT,
                    code: "workspace_apply_conflict",
                    message: detail,
                });
            }
            Err(WorkspaceApplyError::Failure(detail)) => {
                store
                    .update_apply_file(
                        &receipt.apply_id,
                        owner,
                        &file.path,
                        "failed",
                        Some(&detail),
                        "applying",
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(map_store)?;
                store
                    .finish_apply(
                        &receipt.apply_id,
                        owner,
                        "failed",
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(map_store)?;
                return Err(ApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "workspace_apply_failed",
                    message: detail,
                });
            }
        }
    }
    store
        .finish_apply(
            &receipt.apply_id,
            owner,
            "applied",
            chrono::Utc::now().timestamp(),
        )
        .map_err(map_store)
}

fn apply_workspace_file(
    root: &FsPath,
    checkout: &FsPath,
    apply_id: &str,
    change: &execlaw_core::workspaces::WorkspaceApplyFile,
) -> Result<(), WorkspaceApplyError> {
    let (parent, destination) =
        safe_target(root, &change.path, false).map_err(WorkspaceApplyError::Failure)?;
    let backup = parent.join(format!(
        ".execlaw-apply-{}-{}.bak",
        apply_id,
        hex::encode(Sha256::digest(change.path.as_bytes()))[..16].to_owned()
    ));
    let current = read_target(root, &change.path, false).map_err(WorkspaceApplyError::Failure)?;
    let mut current_hash = current
        .as_ref()
        .map(|bytes| hex::encode(Sha256::digest(bytes)));
    if current.is_none() && backup.exists() {
        let metadata = std::fs::symlink_metadata(&backup)
            .map_err(|error| WorkspaceApplyError::Failure(error.to_string()))?;
        if metadata.file_type().is_symlink()
            || is_reparse_point(&metadata)
            || !metadata.is_file()
            || has_multiple_links(&metadata)
        {
            return Err(WorkspaceApplyError::Conflict(format!(
                "rollback file for '{}' is not a safe regular file",
                change.path
            )));
        }
        let bytes = std::fs::read(&backup)
            .map_err(|error| WorkspaceApplyError::Failure(error.to_string()))?;
        current_hash = Some(hex::encode(Sha256::digest(bytes)));
    }
    if current_hash == change.proposed_sha256 {
        if backup.exists() {
            let backup_hash = read_safe_regular_file(&backup)
                .map_err(WorkspaceApplyError::Failure)
                .map(|bytes| hex::encode(Sha256::digest(bytes)))?;
            if Some(backup_hash) != change.base_sha256 {
                return Err(WorkspaceApplyError::Conflict(format!(
                    "rollback receipt for '{}' no longer matches its base hash",
                    change.path
                )));
            }
            std::fs::remove_file(&backup)
                .map_err(|error| WorkspaceApplyError::Failure(error.to_string()))?;
        }
        return Ok(());
    }
    if current_hash != change.base_sha256 {
        return Err(WorkspaceApplyError::Conflict(format!(
            "'{}' changed after the diff was reviewed",
            change.path
        )));
    }
    match &change.proposed_sha256 {
        None => {
            if !backup.exists() {
                std::fs::rename(&destination, &backup).map_err(|error| {
                    WorkspaceApplyError::Failure(format!(
                        "move '{}' to durable rollback receipt: {error}",
                        change.path
                    ))
                })?;
            }
            let backup_hash = read_safe_regular_file(&backup)
                .map_err(WorkspaceApplyError::Failure)
                .map(|bytes| hex::encode(Sha256::digest(bytes)))?;
            if Some(backup_hash) != change.base_sha256 {
                if !destination.exists() {
                    let _ = std::fs::rename(&backup, &destination);
                }
                return Err(WorkspaceApplyError::Conflict(format!(
                    "'{}' changed during deletion; original content was preserved",
                    change.path
                )));
            }
            if destination.exists() {
                return Err(WorkspaceApplyError::Conflict(format!(
                    "'{}' was recreated during deletion; original content was preserved",
                    change.path
                )));
            }
            std::fs::remove_file(&backup).map_err(|error| {
                WorkspaceApplyError::Failure(format!(
                    "remove deleted-file rollback receipt: {error}"
                ))
            })?;
            Ok(())
        }
        Some(proposed_hash) => {
            let proposed_path = safe_relative_file(checkout, &change.path)
                .map_err(|error| WorkspaceApplyError::Failure(error.message))?;
            let bytes = std::fs::read(&proposed_path)
                .map_err(|error| WorkspaceApplyError::Failure(error.to_string()))?;
            if hex::encode(Sha256::digest(&bytes)) != *proposed_hash {
                return Err(WorkspaceApplyError::Conflict(format!(
                    "isolated checkout file '{}' no longer matches the preview",
                    change.path
                )));
            }
            let (parent, destination) =
                safe_target(root, &change.path, true).map_err(WorkspaceApplyError::Failure)?;
            let backup = parent.join(format!(
                ".execlaw-apply-{}-{}.bak",
                apply_id,
                hex::encode(Sha256::digest(change.path.as_bytes()))[..16].to_owned()
            ));
            let mut staged = tempfile::Builder::new()
                .prefix(".execlaw-apply-stage-")
                .tempfile_in(&parent)
                .map_err(|error| WorkspaceApplyError::Failure(error.to_string()))?;
            staged
                .write_all(&bytes)
                .and_then(|_| staged.as_file().sync_all())
                .map_err(|error| WorkspaceApplyError::Failure(error.to_string()))?;
            if change.base_sha256.is_some() && !backup.exists() {
                std::fs::rename(&destination, &backup).map_err(|error| {
                    WorkspaceApplyError::Failure(format!(
                        "stage original '{}' for conditional replace: {error}",
                        change.path
                    ))
                })?;
            }
            if backup.exists() {
                let backup_hash = read_safe_regular_file(&backup)
                    .map_err(WorkspaceApplyError::Failure)
                    .map(|old| hex::encode(Sha256::digest(old)))?;
                if Some(backup_hash) != change.base_sha256 {
                    if !destination.exists() {
                        let _ = std::fs::rename(&backup, &destination);
                    }
                    return Err(WorkspaceApplyError::Conflict(format!(
                        "'{}' changed during apply; original content was preserved",
                        change.path
                    )));
                }
            }
            if change.base_sha256.is_some() && destination.exists() {
                return Err(WorkspaceApplyError::Conflict(format!(
                    "'{}' was recreated during replacement; concurrent content was not overwritten",
                    change.path
                )));
            }
            if let Err(error) = staged.persist_noclobber(&destination) {
                let _ = error;
                if backup.exists() && !destination.exists() {
                    let _ = std::fs::rename(&backup, &destination);
                }
                return Err(WorkspaceApplyError::Conflict(format!(
                    "'{}' was created concurrently; concurrent content was not overwritten",
                    change.path
                )));
            }
            if backup.exists() {
                std::fs::remove_file(&backup).map_err(|error| {
                    WorkspaceApplyError::Failure(format!(
                        "remove replaced-file rollback receipt: {error}"
                    ))
                })?;
            }
            let written =
                read_safe_regular_file(&destination).map_err(WorkspaceApplyError::Failure)?;
            if hex::encode(Sha256::digest(written)) != *proposed_hash {
                return Err(WorkspaceApplyError::Conflict(format!(
                    "'{}' changed immediately after atomic replacement",
                    change.path
                )));
            }
            Ok(())
        }
    }
}

fn read_safe_regular_file(path: &FsPath) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || is_reparse_point(&metadata)
        || has_multiple_links(&metadata)
    {
        return Err("path is not a regular single-link file".into());
    }
    std::fs::read(path).map_err(|error| error.to_string())
}

fn safe_target(
    root: &FsPath,
    relative: &str,
    create_parents: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let rel = PathBuf::from(relative);
    if rel.is_absolute()
        || rel
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || is_secret_path(&rel)
    {
        return Err("path is outside the workspace allowlist".into());
    }
    let components = rel
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect::<Vec<_>>();
    let Some(filename) = components.last() else {
        return Err("empty workspace path".into());
    };
    let mut parent = root.to_path_buf();
    for component in components.iter().take(components.len() - 1) {
        let candidate = parent.join(component);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata)
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && !is_reparse_point(&metadata) =>
            {
                parent = candidate
            }
            Ok(_) => return Err("workspace parent is not a safe directory".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                std::fs::create_dir(&candidate).map_err(|error| error.to_string())?;
                parent = candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((candidate.clone(), candidate.join(filename)));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let canonical_parent = std::fs::canonicalize(&parent).map_err(|error| error.to_string())?;
    if !canonical_parent.starts_with(root) {
        return Err("workspace parent escaped registered root".into());
    }
    let destination = parent.join(filename);
    if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || is_reparse_point(&metadata)
            || has_multiple_links(&metadata)
        {
            return Err("workspace target is not a regular single-link file".into());
        }
    }
    Ok((parent, destination))
}

fn read_target(
    root: &FsPath,
    relative: &str,
    create_parents: bool,
) -> Result<Option<Vec<u8>>, String> {
    let (parent, path) = safe_target(root, relative, create_parents)?;
    if !parent.exists() || !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || is_reparse_point(&metadata)
        || has_multiple_links(&metadata)
    {
        return Err("workspace target is not a regular single-link file".into());
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn compute_workspace_diff(
    state: &AppState,
    run_id: &str,
) -> Result<WorkspaceDiffPreview, ApiError> {
    let store = WorkspaceStore::new(&state.db);
    let binding = store
        .binding_for_run(&run_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_binding_not_found",
            message: "run has no workspace checkpoint binding".into(),
        })?;
    let root = root_path(&state, &binding.workspace_id)?;
    let checkout = std::fs::canonicalize(&binding.checkout_path)
        .map_err(|error| io_error("workspace_checkout_read", error))?;
    let checkout_root = std::fs::canonicalize(state.data_dir.join("workspace-checkouts"))
        .map_err(|error| io_error("workspace_checkout_root", error))?;
    if !checkout.starts_with(&checkout_root) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_checkout_escape",
            message: "run checkout resolves outside the managed workspace directory".into(),
        });
    }
    let checkpoint = store
        .get_checkpoint(&binding.checkpoint_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_checkpoint_not_found",
            message: "bound workspace checkpoint was not found".into(),
        })?;
    let base = checkpoint
        .manifest
        .iter()
        .map(|entry| (entry.path.clone(), entry.sha256.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let current = hash_files(scan_workspace(&root)?);
    let proposed = hash_files(scan_workspace(&checkout)?);
    let paths = base
        .keys()
        .chain(current.keys())
        .chain(proposed.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut changes = Vec::new();
    for path in paths {
        let base_hash = base.get(&path);
        let proposed_hash = proposed.get(&path);
        if base_hash == proposed_hash {
            continue;
        }
        let current_hash = current.get(&path);
        let already_applied = current_hash == proposed_hash;
        let conflict = !already_applied && current_hash != base_hash;
        changes.push(WorkspaceDiffChange {
            path,
            base_sha256: base_hash.cloned(),
            current_sha256: current_hash.cloned(),
            proposed_sha256: proposed_hash.cloned(),
            conflict,
        });
    }
    let conflicts = changes.iter().filter(|change| change.conflict).count();
    let fingerprint = serde_json::to_vec(&serde_json::json!({"run_id":run_id,"workspace_id":binding.workspace_id,"checkpoint_id":binding.checkpoint_id,"changes":changes}))
        .map_err(|error| ApiError {status:StatusCode::INTERNAL_SERVER_ERROR,code:"workspace_diff_encode",message:error.to_string()})?;
    let preview_hash = hex::encode(Sha256::digest(fingerprint));
    Ok(WorkspaceDiffPreview {
        run_id: run_id.to_owned(),
        workspace_id: binding.workspace_id,
        checkpoint_id: binding.checkpoint_id,
        preview_hash,
        changes,
        conflicts,
    })
}

fn hash_files(files: Vec<(String, Vec<u8>)>) -> std::collections::BTreeMap<String, String> {
    files
        .into_iter()
        .map(|(path, bytes)| (path, hex::encode(Sha256::digest(bytes))))
        .collect()
}

fn root_path(state: &AppState, workspace_id: &str) -> Result<PathBuf, ApiError> {
    let root = WorkspaceStore::new(&state.db)
        .get_root(workspace_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_not_found",
            message: "registered workspace root not found".into(),
        })?;
    let canonical = std::fs::canonicalize(&root.canonical_path)
        .map_err(|error| io_error("workspace_root_access", error))?;
    if !canonical.is_dir() || canonical.to_string_lossy() != root.canonical_path {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_root_changed",
            message: "registered workspace root no longer resolves to the same canonical directory"
                .into(),
        });
    }
    Ok(canonical)
}

fn safe_relative_file(root: &FsPath, relative: &str) -> Result<PathBuf, ApiError> {
    let path = PathBuf::from(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || is_secret_path(&path)
    {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_path_denied",
            message: "absolute, traversal, and secret paths are not readable".into(),
        });
    }
    let mut current = root.to_path_buf();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| io_error("workspace_path_read", error))?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                code: "workspace_link_denied",
                message: "workspace paths may not cross symbolic links or junctions".into(),
            });
        }
    }
    let canonical = std::fs::canonicalize(&current)
        .map_err(|error| io_error("workspace_path_canonicalize", error))?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_path_denied",
            message: "workspace file resolves outside its registered root".into(),
        });
    }
    Ok(canonical)
}

fn scan_workspace(root: &FsPath) -> Result<Vec<(String, Vec<u8>)>, ApiError> {
    const IGNORED_DIRS: &[&str] = &[".git", ".execlaw", "node_modules", "target", ".venv"];
    let canonical_root =
        std::fs::canonicalize(root).map_err(|error| io_error("workspace_snapshot_root", error))?;
    let mut stack = vec![(canonical_root.clone(), String::new(), 0usize)];
    let mut files = Vec::new();
    let mut total_bytes = 0u64;
    while let Some((directory, relative, depth)) = stack.pop() {
        if depth > 64 {
            return Err(ApiError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "workspace_depth_limit",
                message: "workspace exceeds 64 directory levels".into(),
            });
        }
        let mut entries = std::fs::read_dir(&directory)
            .map_err(|error| io_error("workspace_snapshot_read", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io_error("workspace_snapshot_read", error))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.is_empty() || is_secret_component(&name) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| io_error("workspace_snapshot_metadata", error))?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(ApiError {
                    status: StatusCode::FORBIDDEN,
                    code: "workspace_link_denied",
                    message: format!("snapshot contains symbolic link or junction '{name}'"),
                });
            }
            if metadata.is_file() && has_multiple_links(&metadata) {
                return Err(ApiError {
                    status: StatusCode::FORBIDDEN,
                    code: "workspace_hardlink_denied",
                    message: format!("snapshot contains a multiply-linked file '{name}'"),
                });
            }
            if metadata.is_dir() {
                if !IGNORED_DIRS.contains(&name.to_ascii_lowercase().as_str()) {
                    let rel = join_relative(&relative, &name);
                    stack.push((entry.path(), rel, depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if metadata.len() > MAX_FILE_BYTES {
                return Err(ApiError {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "workspace_file_too_large",
                    message: format!("file '{name}' exceeds the 10 MiB snapshot limit"),
                });
            }
            let path = join_relative(&relative, &name);
            let file_path = safe_relative_file(&canonical_root, &path)?;
            let bytes = std::fs::read(file_path)
                .map_err(|error| io_error("workspace_snapshot_file", error))?;
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            if total_bytes > MAX_SNAPSHOT_BYTES || files.len() >= 10_000 {
                return Err(ApiError {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "workspace_snapshot_too_large",
                    message: "workspace snapshot exceeds 100 MiB or 10000 files".into(),
                });
            }
            files.push((path, bytes));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

pub(crate) fn clone_workspace_for_run(
    state: &AppState,
    source_run_id: &str,
    fork_run_id: &str,
) -> Result<(), ApiError> {
    let store = WorkspaceStore::new(&state.db);
    let Some(binding) = store.binding_for_run(source_run_id).map_err(map_store)? else {
        return Ok(());
    };
    let checkpoint = store
        .get_checkpoint(&binding.checkpoint_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_checkpoint_missing",
            message: "source run workspace checkpoint is missing".into(),
        })?;
    let checkout = state
        .data_dir
        .join("workspace-checkouts")
        .join(hex::encode(Sha256::digest(
            format!("{fork_run_id}:{}", checkpoint.checkpoint_id).as_bytes(),
        )));
    materialize_checkpoint(&store, &checkpoint, &checkout)?;
    store
        .bind_run_checkout(
            fork_run_id,
            &binding.workspace_id,
            &checkpoint.checkpoint_id,
            &checkout.to_string_lossy(),
            chrono::Utc::now().timestamp(),
        )
        .map_err(map_store)?;
    Ok(())
}

fn materialize_checkpoint(
    store: &WorkspaceStore<'_>,
    checkpoint: &WorkspaceCheckpointRecord,
    destination: &FsPath,
) -> Result<(), ApiError> {
    std::fs::create_dir_all(destination)
        .map_err(|error| io_error("workspace_checkout_create", error))?;
    let root = std::fs::canonicalize(destination)
        .map_err(|error| io_error("workspace_checkout_canonicalize", error))?;
    for entry in &checkpoint.manifest {
        let relative = PathBuf::from(&entry.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || is_secret_path(&relative)
        {
            return Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "workspace_manifest_invalid",
                message: "stored checkpoint contains an invalid path".into(),
            });
        }
        let path = root.join(&relative);
        let parent = path.parent().ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "workspace_manifest_invalid",
            message: "checkpoint path has no parent".into(),
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| io_error("workspace_checkout_directory", error))?;
        let bytes = store
            .read_blob(&entry.sha256)
            .map_err(map_workspace_error)?
            .ok_or_else(|| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "workspace_blob_missing",
                message: "checkpoint content blob is missing".into(),
            })?;
        std::fs::write(&path, &bytes)
            .map_err(|error| io_error("workspace_checkout_file", error))?;
    }
    Ok(())
}

fn join_relative(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

fn is_secret_path(path: &FsPath) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(is_secret_component)
    })
}

fn is_secret_component(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.starts_with(".execlaw-apply-")
        || matches!(
            lower.as_str(),
            ".ssh"
                | ".aws"
                | ".azure"
                | ".kube"
                | ".execlaw"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
                | "id_rsa"
                | "id_ecdsa"
                | "id_ed25519"
                | "credentials"
                | "credential"
                | "password"
                | "passwd"
                | "token"
                | "config.json"
        )
        || lower.contains("credential")
        || lower.contains("secret")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".keystore")
        || lower.ends_with(".kubeconfig")
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn has_multiple_links(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() > 1
}

#[cfg(windows)]
fn has_multiple_links(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn io_error(code: &'static str, error: std::io::Error) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code,
        message: error.to_string(),
    }
}
fn map_workspace_error(error: execlaw_core::workspaces::WorkspaceStoreError) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "workspace_checkpoint_error",
        message: error.to_string(),
    }
}
