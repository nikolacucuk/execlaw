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
        WorkspaceApplyReceipt, WorkspaceCheckpointRecord, WorkspacePatchClaim, WorkspaceRootRecord,
        WorkspaceStore,
    },
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path as FsPath, PathBuf};

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 500;
const CODE_SOURCE_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "rhai", "md", "mdx"];

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

#[derive(Debug, Deserialize)]
pub struct WorkspaceCodeQuery {
    pub query: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceExecutionConfigRequest {
    pub image_reference: String,
    pub language_servers: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub approve_image_digest: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkspacePatchRequest {
    pub edits: Vec<WorkspaceFileEdit>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkspaceFileEdit {
    pub path: String,
    /// SHA-256 of the current checkout file, or null when creating a file.
    pub expected_sha256: Option<String>,
    pub content: String,
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
        .route(
            "/api/admin/workspaces/{workspace_id}/code-index",
            get(code_index_status).post(index_workspace),
        )
        .route(
            "/api/admin/workspaces/{workspace_id}/code-index/symbols",
            get(search_code_symbols),
        )
        .route("/api/admin/workspaces/{workspace_id}/files", get(read_file))
        .route(
            "/api/admin/workspaces/{workspace_id}/search",
            post(search_files),
        )
        .route(
            "/api/admin/runs/{run_id}/workspace-patch",
            post(patch_run_workspace),
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
        .route(
            "/api/admin/workspace-execution",
            get(get_execution_config).put(set_execution_config),
        )
}

/// Read the Controller-selected, provenance-verified workspace toolchain.
#[utoipa::path(
    get,
    path = "/api/admin/workspace-execution",
    responses((status = 200, description = "Workspace execution configuration", body = serde_json::Value)),
    tag = "workspaces"
)]
pub async fn get_execution_config(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    let config = WorkspaceStore::new(&state.db)
        .execution_config()
        .map_err(map_store)?;
    Ok(Json(serde_json::json!({
        "image_reference":config.image_reference,
        "language_servers":config.language_servers,
        "updated_at":config.updated_at,
        "updated_by":config.updated_by
    })))
}

/// Configure an installed, digest-pinned toolchain and its generic LSP commands.
#[utoipa::path(
    put,
    path = "/api/admin/workspace-execution",
    request_body = serde_json::Value,
    responses((status = 200, description = "Workspace execution configuration saved", body = serde_json::Value)),
    tag = "workspaces"
)]
pub async fn set_execution_config(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<WorkspaceExecutionConfigRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    let provenance =
        execlaw_core::artifact_provenance::ArtifactProvenanceStore::new(state.db.clone());
    let valid_reference = is_digest_pinned_oci_reference(&request.image_reference)
        || (request.approve_image_digest && is_local_image_id(&request.image_reference));
    if !valid_reference {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_toolchain_digest_required",
            message: "workspace toolchain must use an OCI digest; local image IDs require the explicit Controller development override".into(),
        });
    }
    let artifact_type = execlaw_core::artifact_provenance::ArtifactType::Sidecar;
    let mut controller_approval_recorded = false;
    if let Err(error) = provenance.authorize_oci_reference(
        "sidecar:workspace-toolchain",
        artifact_type,
        &request.image_reference,
        &user.username,
    ) {
        if !request.approve_image_digest {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                code: "workspace_toolchain_provenance_required",
                message: error.to_string(),
            });
        }
        provenance
            .approve_controller_oci_reference(
                artifact_type,
                &request.image_reference,
                "Controller",
                &user.username,
            )
            .map_err(|error| ApiError {
                status: StatusCode::FORBIDDEN,
                code: "workspace_toolchain_approval_failed",
                message: error.to_string(),
            })?;
        controller_approval_recorded = true;
    }
    let saved = WorkspaceStore::new(&state.db)
        .set_execution_config(
            &user.username,
            &request.image_reference,
            &request.language_servers,
            chrono::Utc::now().timestamp(),
        )
        .map_err(map_store)?;
    Ok(Json(serde_json::json!({
        "image_reference":saved.image_reference,
        "language_servers":saved.language_servers,
        "approval":if controller_approval_recorded { "controller_digest_approval" } else { "verified_or_prior_controller_approval" },
        "updated_at":saved.updated_at,
        "updated_by":saved.updated_by
    })))
}

fn is_digest_pinned_oci_reference(reference: &str) -> bool {
    let Some((name, digest)) = reference.rsplit_once("@sha256:") else {
        return false;
    };
    !name.trim().is_empty()
        && digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_local_image_id(reference: &str) -> bool {
    reference.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
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
    let (status, code) = match &error {
        execlaw_core::workspaces::WorkspaceStoreError::Invalid(_) => {
            (StatusCode::BAD_REQUEST, "workspace_error")
        }
        execlaw_core::workspaces::WorkspaceStoreError::Conflict(_) => {
            (StatusCode::CONFLICT, "workspace_conflict")
        }
        execlaw_core::workspaces::WorkspaceStoreError::Db(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "workspace_error")
        }
    };
    ApiError {
        status,
        code,
        message: error.to_string(),
    }
}

fn map_db(error: execlaw_core::DbError) -> ApiError {
    tracing::error!(error = %error, "workspace database operation failed");
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "workspace_database_error",
        message: "workspace database operation failed".into(),
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
    if has_multiple_links(&path, &metadata) {
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

/// Incrementally index source and documentation symbols for one registered root.
/// A text search remains the fallback whenever this receipt is stale or incomplete.
pub async fn index_workspace(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(workspace_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    let root = root_path(&state, &workspace_id)?;
    let files = scan_workspace(&root)?;
    let mut indexed = Vec::new();
    let mut unsupported = 0usize;
    for (path, bytes) in &files {
        let extension = FsPath::new(path)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !CODE_SOURCE_EXTENSIONS.contains(&extension.as_str()) {
            unsupported += 1;
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            unsupported += 1;
            continue;
        };
        indexed.push((
            path.as_str(),
            hex::encode(Sha256::digest(bytes)),
            code_symbols(text),
        ));
    }
    let excluded = count_excluded_paths(&root)?;
    let mut revision_input = Vec::new();
    for (path, bytes) in &files {
        revision_input.extend_from_slice(path.as_bytes());
        revision_input.push(0);
        revision_input.extend_from_slice(hex::encode(Sha256::digest(bytes)).as_bytes());
        revision_input.push(b'\n');
    }
    let revision = hex::encode(Sha256::digest(&revision_input));
    let now = chrono::Utc::now().timestamp();
    state.db.transaction(|tx| {
        tx.execute("INSERT INTO state_workspace_code_indexes(workspace_id,revision,files_indexed,files_excluded,files_unsupported,updated_at) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(workspace_id) DO UPDATE SET revision=excluded.revision,files_indexed=excluded.files_indexed,files_excluded=excluded.files_excluded,files_unsupported=excluded.files_unsupported,updated_at=excluded.updated_at", rusqlite::params![workspace_id, revision, indexed.len() as i64, excluded as i64, unsupported as i64, now])?;
        let paths = indexed.iter().map(|(path, _, _)| *path).collect::<std::collections::HashSet<_>>();
        let mut old_paths = tx.prepare("SELECT path FROM state_workspace_code_files WHERE workspace_id=?1")?;
        let old = old_paths.query_map([&workspace_id], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        drop(old_paths);
        for path in old {
            if !paths.contains(path.as_str()) {
                tx.execute("DELETE FROM state_workspace_code_files WHERE workspace_id=?1 AND path=?2", rusqlite::params![workspace_id, path])?;
                tx.execute("DELETE FROM state_workspace_code_symbols WHERE workspace_id=?1 AND path=?2", rusqlite::params![workspace_id, path])?;
            }
        }
        for (path, hash, symbols) in &indexed {
            let previous: Option<String> = tx.query_row("SELECT source_hash FROM state_workspace_code_files WHERE workspace_id=?1 AND path=?2", rusqlite::params![workspace_id, path], |row| row.get(0)).optional()?;
            if previous.as_deref() == Some(hash.as_str()) { continue; }
            tx.execute("DELETE FROM state_workspace_code_symbols WHERE workspace_id=?1 AND path=?2", rusqlite::params![workspace_id, path])?;
            tx.execute("INSERT INTO state_workspace_code_files(workspace_id,path,source_hash) VALUES (?1,?2,?3) ON CONFLICT(workspace_id,path) DO UPDATE SET source_hash=excluded.source_hash", rusqlite::params![workspace_id, path, hash])?;
            for (symbol, kind, line, end_line) in symbols {
                tx.execute("INSERT INTO state_workspace_code_symbols(workspace_id,path,symbol,kind,start_line,end_line,source_hash) VALUES (?1,?2,?3,?4,?5,?6,?7)", rusqlite::params![workspace_id,path,symbol,kind,line,end_line,hash])?;
            }
        }
        Ok(())
    }).map_err(map_db)?;
    Ok(Json(
        serde_json::json!({"workspace_id":workspace_id,"revision":revision,"fresh":true,"coverage":{"indexed":indexed.len(),"excluded":excluded,"unsupported":unsupported}}),
    ))
}

/// Return index freshness before callers rely on symbol or impact results.
pub async fn code_index_status(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(workspace_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    let root = root_path(&state, &workspace_id)?;
    let files = scan_workspace(&root)?;
    let mut current_input = Vec::new();
    for (path, bytes) in &files {
        current_input.extend_from_slice(path.as_bytes());
        current_input.push(0);
        current_input.extend_from_slice(hex::encode(Sha256::digest(bytes)).as_bytes());
        current_input.push(b'\n');
    }
    let current_revision = hex::encode(Sha256::digest(&current_input));
    let receipt = state.db.with_conn(|c| Ok(c.query_row("SELECT revision,files_indexed,files_excluded,files_unsupported,updated_at FROM state_workspace_code_indexes WHERE workspace_id=?1", [&workspace_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?))).optional()?)).map_err(map_db)?;
    let Some((revision, indexed, excluded, unsupported, updated_at)) = receipt else {
        return Ok(Json(
            serde_json::json!({"workspace_id":workspace_id,"indexed":false,"fresh":false,"reason":"not_indexed"}),
        ));
    };
    Ok(Json(
        serde_json::json!({"workspace_id":workspace_id,"indexed":true,"fresh":revision == current_revision,"revision":revision,"current_revision":current_revision,"coverage":{"indexed":indexed,"excluded":excluded,"unsupported":unsupported},"updated_at":updated_at}),
    ))
}

fn code_symbols(text: &str) -> Vec<(String, String, i64, i64)> {
    let declaration = regex::Regex::new(r"(?m)^\s*(?:(?:pub(?:\([^)]*\))?|export|async)\s+)*(fn|struct|enum|trait|type|const|static|mod|class|interface|function)\s+([A-Za-z_$][A-Za-z0-9_$]*)").expect("static declaration regex is valid");
    declaration
        .captures_iter(text)
        .filter_map(|capture| {
            let whole = capture.get(0)?;
            let line = text[..whole.start()]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count() as i64
                + 1;
            Some((
                capture.get(2)?.as_str().to_owned(),
                capture.get(1)?.as_str().to_owned(),
                line,
                line,
            ))
        })
        .collect()
}

fn count_excluded_paths(root: &FsPath) -> Result<usize, ApiError> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut count = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if depth >= 64 {
            continue;
        }
        for entry in std::fs::read_dir(directory)
            .map_err(|error| io_error("workspace_index_coverage", error))?
        {
            let entry = entry.map_err(|error| io_error("workspace_index_coverage", error))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if is_secret_component(&name) {
                count = count.saturating_add(1);
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| io_error("workspace_index_coverage", error))?;
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && !is_reparse_point(&metadata)
            {
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(count)
}

/// Search indexed declarations only while the index matches the current workspace revision.
pub async fn search_code_symbols(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(workspace_id): Path<String>,
    Query(query): Query<WorkspaceCodeQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    if query.query.trim().is_empty() || query.query.len() > 256 {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_code_query_invalid",
            message: "symbol query must contain 1 to 256 bytes".into(),
        });
    }
    let status = code_index_status(State(state.clone()), user, Path(workspace_id.clone()))
        .await?
        .0;
    if status["fresh"] != true {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_code_index_stale",
            message: "refresh the workspace code index before relying on symbol results".into(),
        });
    }
    let needle = format!("%{}%", query.query.replace('%', "").replace('_', ""));
    let results = state.db.with_conn(|c| {
        let mut statement = c.prepare("SELECT path,symbol,kind,start_line,end_line,source_hash FROM state_workspace_code_symbols WHERE workspace_id=?1 AND symbol LIKE ?2 ORDER BY symbol,path,start_line LIMIT 200")?;
        let rows = statement.query_map(rusqlite::params![workspace_id, needle], |r| Ok(serde_json::json!({"path":r.get::<_,String>(0)?,"symbol":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"start_line":r.get::<_,i64>(3)?,"end_line":r.get::<_,i64>(4)?,"source_hash":r.get::<_,String>(5)?})))?;
        Ok(rows.collect::<Result<Vec<_>,_>>()?)
    }).map_err(map_db)?;
    Ok(Json(serde_json::json!({"fresh":true,"results":results})))
}

/// Apply bounded, hash-checked file replacements inside a run's isolated checkout.
/// The registered workspace root remains untouched until a reviewed diff is applied.
#[utoipa::path(
    post,
    path = "/api/admin/runs/{run_id}/workspace-patch",
    params(("run_id" = String, Path, description = "Run whose isolated workspace checkout will be edited"), ("Idempotency-Key" = String, Header, description = "Retry identity for this durable patch job")),
    request_body = serde_json::Value,
    responses((status = 200, description = "Hash-checked workspace edit results", body = serde_json::Value)),
    tag = "workspaces"
)]
pub async fn patch_run_workspace(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<WorkspacePatchRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    if request.edits.is_empty() || request.edits.len() > 64 {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_patch_size_invalid",
            message: "workspace patch must contain 1 to 64 file edits".into(),
        });
    }
    let mut total_bytes = 0u64;
    let mut paths = std::collections::HashSet::new();
    for edit in &request.edits {
        if edit.content.len() as u64 > MAX_FILE_BYTES {
            return Err(ApiError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "workspace_file_too_large",
                message: "workspace patch file exceeds the 10 MiB limit".into(),
            });
        }
        total_bytes = total_bytes.saturating_add(edit.content.len() as u64);
        if total_bytes > MAX_SNAPSHOT_BYTES {
            return Err(ApiError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "workspace_patch_too_large",
                message: "workspace patch exceeds the 100 MiB limit".into(),
            });
        }
        if !paths.insert(edit.path.as_str()) {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "workspace_patch_duplicate_path",
                message: "workspace patch contains duplicate paths".into(),
            });
        }
        if edit.expected_sha256.as_ref().is_some_and(|hash| {
            hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "workspace_patch_hash_invalid",
                message: "expected_sha256 must be a 64-character hexadecimal digest".into(),
            });
        }
    }

    let request_id = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_patch_idempotency_required",
            message: "Idempotency-Key header is required".into(),
        })?;
    let serialized_request = serde_json::to_vec(&request).map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "workspace_patch_request_invalid",
        message: error.to_string(),
    })?;
    let request_hash = hex::encode(Sha256::digest(&serialized_request));
    let lease_owner = uuid::Uuid::new_v4().to_string();
    let store = WorkspaceStore::new(&state.db);
    match store
        .begin_patch_job(
            &run_id,
            request_id,
            &request_hash,
            &lease_owner,
            chrono::Utc::now().timestamp(),
        )
        .map_err(map_store)?
    {
        WorkspacePatchClaim::Succeeded(result) => {
            return serde_json::from_str(&result)
                .map(Json)
                .map_err(|error| ApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "workspace_patch_receipt_invalid",
                    message: error.to_string(),
                });
        }
        WorkspacePatchClaim::Failed(code) => {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_patch_failed",
                message: format!("a prior attempt for this request ended with {code}"),
            });
        }
        WorkspacePatchClaim::Busy => {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_patch_busy",
                message: "another request is processing this idempotency key".into(),
            });
        }
        WorkspacePatchClaim::Claimed => {}
    }
    let mut lease = WorkspacePatchLease::new(state.db.clone(), &run_id, request_id, &lease_owner);

    let binding = store
        .binding_for_run(&run_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "workspace_binding_not_found",
            message: "run has no isolated workspace checkout".into(),
        })?;
    let checkout_root = std::fs::canonicalize(state.data_dir.join("workspace-checkouts"))
        .map_err(|error| io_error("workspace_checkout_root", error))?;
    let checkout = std::fs::canonicalize(&binding.checkout_path)
        .map_err(|error| io_error("workspace_checkout_access", error))?;
    if !checkout.is_dir() || !checkout.starts_with(&checkout_root) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_checkout_escape",
            message: "run checkout is outside the managed workspace directory".into(),
        });
    }

    let results = apply_checkout_patch(&checkout, &request.edits)?;
    let result = serde_json::json!({"run_id": run_id, "request_id": request_id, "edits": results});
    let result_json = serde_json::to_string(&result).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "workspace_patch_result_encode_failed",
        message: error.to_string(),
    })?;
    lease.finish(Some(&result_json), None)?;
    Ok(Json(result))
}

fn restore_patch_backup(backup_path: tempfile::TempPath, target: &FsPath) -> Option<String> {
    if !target.exists() && std::fs::rename(&backup_path, target).is_ok() {
        return None;
    }
    backup_path
        .keep()
        .ok()
        .map(|path| path.display().to_string())
}

pub(crate) fn apply_checkout_edit(
    checkout: &FsPath,
    edit: &WorkspaceFileEdit,
) -> Result<serde_json::Value, ApiError> {
    let target = safe_target(checkout, &edit.path, true)
        .map_err(|message| ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_patch_path_denied",
            message,
        })?
        .1;
    let current = read_target(checkout, &edit.path, false).map_err(|message| ApiError {
        status: StatusCode::FORBIDDEN,
        code: "workspace_patch_path_denied",
        message,
    })?;
    let original_permissions = if current.is_some() {
        Some(
            std::fs::metadata(&target)
                .map_err(|error| io_error("workspace_patch_metadata", error))?
                .permissions(),
        )
    } else {
        None
    };
    let current_hash = current
        .as_ref()
        .map(|bytes| hex::encode(Sha256::digest(bytes)));
    let content = edit.content.as_bytes();
    let next_hash = hex::encode(Sha256::digest(content));
    if current_hash.as_deref() == Some(next_hash.as_str()) {
        return Ok(serde_json::json!({"path": edit.path, "sha256": next_hash}));
    }
    if current_hash.as_deref() != edit.expected_sha256.as_deref() {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "workspace_patch_conflict",
            message: format!("workspace file changed since it was read: {}", edit.path),
        });
    }
    let parent = target.parent().ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "workspace_patch_path_denied",
        message: "workspace patch target has no parent directory".into(),
    })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| io_error("workspace_patch_temp_create", error))?;
    temp.write_all(content)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|error| io_error("workspace_patch_write", error))?;
    if let Some(permissions) = original_permissions {
        temp.as_file()
            .set_permissions(permissions)
            .map_err(|error| io_error("workspace_patch_permissions", error))?;
    }
    let mut backup = None;
    if current.is_some() {
        let placeholder = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| io_error("workspace_patch_backup_create", error))?;
        let backup_path = placeholder.into_temp_path();
        std::fs::remove_file(&backup_path)
            .map_err(|error| io_error("workspace_patch_backup_stage", error))?;
        std::fs::rename(&target, &backup_path)
            .map_err(|error| io_error("workspace_patch_backup", error))?;
        let moved_hash = match std::fs::read(&backup_path) {
            Ok(bytes) => hex::encode(Sha256::digest(bytes)),
            Err(error) => {
                let preserved = restore_patch_backup(backup_path, &target);
                return Err(io_error(
                    "workspace_patch_backup_read",
                    std::io::Error::other(format!(
                        "{error}; backup {}",
                        preserved.unwrap_or_else(|| "restored".into())
                    )),
                ));
            }
        };
        if edit.expected_sha256.as_deref() != Some(moved_hash.as_str()) {
            let preserved = restore_patch_backup(backup_path, &target);
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_patch_conflict",
                message: format!(
                    "workspace file changed while patching: {}{}",
                    edit.path,
                    preserved
                        .map(|path| format!("; original preserved at {path}"))
                        .unwrap_or_default()
                ),
            });
        }
        backup = Some(backup_path);
    }
    if let Err(error) = temp.persist_noclobber(&target) {
        if let Some(backup_path) = backup.take()
            && let Some(preserved) = restore_patch_backup(backup_path, &target)
        {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_patch_conflict",
                message: format!(
                    "workspace target appeared during patching; original content was preserved at {preserved}"
                ),
            });
        }
        return Err(io_error("workspace_patch_replace", error.error));
    }
    if let Some(backup_path) = backup {
        std::fs::remove_file(backup_path)
            .map_err(|error| io_error("workspace_patch_backup_remove", error))?;
    }
    Ok(serde_json::json!({"path": edit.path, "sha256": next_hash}))
}

/// Apply a bounded multi-file patch after checking every precondition.
///
/// Preflight ensures a conflict in a later edit cannot leave earlier edits
/// committed. If an I/O error occurs during replacement, edits from this call
/// are rolled back only while their proposed hashes still match, preserving
/// any concurrent human change. Replayed batches accept files already at
/// their proposed hash and finish the remaining edits idempotently.
pub(crate) fn apply_checkout_patch(
    checkout: &FsPath,
    edits: &[WorkspaceFileEdit],
) -> Result<Vec<serde_json::Value>, ApiError> {
    const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
    if edits.is_empty() || edits.len() > 64 {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_patch_bounds",
            message: "workspace patch must contain 1 to 64 file edits".into(),
        });
    }
    let total_bytes = edits
        .iter()
        .try_fold(0usize, |total, edit| total.checked_add(edit.content.len()));
    if total_bytes.is_none_or(|total| total > MAX_PATCH_BYTES) {
        return Err(ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "workspace_patch_bounds",
            message: "workspace patch exceeds the 8 MiB total content limit".into(),
        });
    }

    let mut seen = std::collections::HashSet::new();
    let mut originals = Vec::with_capacity(edits.len());
    for edit in edits {
        let (_, target) = safe_target(checkout, &edit.path, true).map_err(|message| ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_patch_path_denied",
            message,
        })?;
        if !seen.insert(target) {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "workspace_patch_duplicate_path",
                message: format!("workspace patch lists '{}' more than once", edit.path),
            });
        }
        let current = read_target(checkout, &edit.path, false).map_err(|message| ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_patch_path_denied",
            message,
        })?;
        if let Some(bytes) = &current {
            String::from_utf8(bytes.clone()).map_err(|_| ApiError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "workspace_patch_encoding_unsupported",
                message: format!("workspace patch target is not UTF-8 text: {}", edit.path),
            })?;
        }
        let current_hash = current
            .as_ref()
            .map(|bytes| hex::encode(Sha256::digest(bytes)));
        let proposed_hash = hex::encode(Sha256::digest(edit.content.as_bytes()));
        if current_hash.as_deref() != Some(proposed_hash.as_str())
            && current_hash.as_deref() != edit.expected_sha256.as_deref()
        {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "workspace_patch_conflict",
                message: format!("workspace file changed since it was read: {}", edit.path),
            });
        }
        originals.push(current);
    }

    let mut results = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        match apply_checkout_edit(checkout, edit) {
            Ok(result) => results.push(result),
            Err(error) => {
                let mut rollback_errors = Vec::new();
                for rollback_index in (0..index).rev() {
                    let applied = &edits[rollback_index];
                    let proposed_hash = hex::encode(Sha256::digest(applied.content.as_bytes()));
                    let target = match safe_target(checkout, &applied.path, true) {
                        Ok((_, target)) => target,
                        Err(message) => {
                            rollback_errors.push(format!("{}: {message}", applied.path));
                            continue;
                        }
                    };
                    let current = match read_target(checkout, &applied.path, false) {
                        Ok(current) => current,
                        Err(message) => {
                            rollback_errors.push(format!("{}: {message}", applied.path));
                            continue;
                        }
                    };
                    if current
                        .as_ref()
                        .map(|bytes| hex::encode(Sha256::digest(bytes)))
                        != Some(proposed_hash.clone())
                    {
                        rollback_errors.push(format!(
                            "{}: changed concurrently; left untouched",
                            applied.path
                        ));
                        continue;
                    }
                    match originals[rollback_index].as_deref() {
                        Some(original_bytes) => {
                            let original = String::from_utf8(original_bytes.to_vec())
                                .expect("preflight rejected non-UTF-8 patch targets");
                            let restore = WorkspaceFileEdit {
                                path: applied.path.clone(),
                                expected_sha256: Some(proposed_hash),
                                content: original,
                            };
                            if let Err(restore_error) = apply_checkout_edit(checkout, &restore) {
                                rollback_errors
                                    .push(format!("{}: {}", applied.path, restore_error.message));
                            }
                        }
                        None => {
                            if let Err(remove_error) = std::fs::remove_file(&target) {
                                rollback_errors.push(format!("{}: {remove_error}", applied.path));
                            }
                        }
                    }
                }
                if rollback_errors.is_empty() {
                    return Err(error);
                }
                return Err(ApiError {
                    status: error.status,
                    code: error.code,
                    message: format!(
                        "{}; rollback incomplete: {}",
                        error.message,
                        rollback_errors.join("; ")
                    ),
                });
            }
        }
    }
    Ok(results)
}

struct WorkspacePatchLease {
    db: execlaw_core::Database,
    run_id: String,
    request_id: String,
    owner: String,
    finished: bool,
}

impl WorkspacePatchLease {
    fn new(db: execlaw_core::Database, run_id: &str, request_id: &str, owner: &str) -> Self {
        Self {
            db,
            run_id: run_id.to_owned(),
            request_id: request_id.to_owned(),
            owner: owner.to_owned(),
            finished: false,
        }
    }

    fn finish(
        &mut self,
        result_json: Option<&str>,
        error_code: Option<&str>,
    ) -> Result<(), ApiError> {
        WorkspaceStore::new(&self.db)
            .finish_patch_job(
                &self.run_id,
                &self.request_id,
                &self.owner,
                result_json,
                error_code,
                chrono::Utc::now().timestamp(),
            )
            .map_err(map_store)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for WorkspacePatchLease {
    fn drop(&mut self) {
        if !self.finished {
            let _ = WorkspaceStore::new(&self.db).finish_patch_job(
                &self.run_id,
                &self.request_id,
                &self.owner,
                None,
                Some("workspace_patch_interrupted"),
                chrono::Utc::now().timestamp(),
            );
        }
    }
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

#[derive(Debug)]
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
            || has_multiple_links(&backup, &metadata)
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
        || has_multiple_links(path, &metadata)
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
    let canonical_root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
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
    if !canonical_parent.starts_with(&canonical_root) {
        return Err("workspace parent escaped registered root".into());
    }
    let destination = parent.join(filename);
    if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || is_reparse_point(&metadata)
            || has_multiple_links(&destination, &metadata)
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
        || has_multiple_links(&path, &metadata)
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

pub(crate) fn safe_relative_file(root: &FsPath, relative: &str) -> Result<PathBuf, ApiError> {
    let canonical_root =
        std::fs::canonicalize(root).map_err(|error| io_error("workspace_path_root", error))?;
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
    let mut current = canonical_root.clone();
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
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "workspace_path_denied",
            message: "workspace file resolves outside its registered root".into(),
        });
    }
    Ok(canonical)
}

pub(crate) fn scan_workspace(root: &FsPath) -> Result<Vec<(String, Vec<u8>)>, ApiError> {
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
            if metadata.is_file() && has_multiple_links(&entry.path(), &metadata) {
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

pub(crate) fn workspace_content_revision(root: &FsPath) -> Result<String, ApiError> {
    let mut digest = Sha256::new();
    for (path, contents) in scan_workspace(root)? {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(Sha256::digest(contents));
        digest.update([b'\n']);
    }
    Ok(hex::encode(digest.finalize()))
}

pub(crate) fn is_test_source_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase().replace('\\', "/");
    let filename = lower.rsplit('/').next().unwrap_or(&lower);
    lower
        .split('/')
        .any(|part| part == "tests" || part == "__tests__")
        || filename.starts_with("test_")
        || filename.ends_with("_test.rs")
        || filename.ends_with(".test.ts")
        || filename.ends_with(".test.tsx")
        || filename.ends_with(".spec.ts")
        || filename.ends_with(".spec.tsx")
        || filename.ends_with("_test.py")
}

pub(crate) fn observed_test_skips(output: &str) -> u32 {
    let pattern = regex::Regex::new(r"(?i)(\d+)\s+(?:ignored|skipped)").expect("static skip regex");
    pattern
        .captures_iter(output)
        .filter_map(|capture| capture.get(1)?.as_str().parse::<u32>().ok())
        .fold(0_u32, u32::saturating_add)
}

pub(crate) fn workspace_execution_evidence(
    command: &[String],
    exit_code: Option<i64>,
    timed_out: bool,
    output_truncated: bool,
    output: &str,
    checked_revision: &str,
    current_revision: &str,
    environment_identity: &str,
    baseline_tests: &std::collections::BTreeMap<String, String>,
    current_files: &std::collections::BTreeMap<String, String>,
) -> serde_json::Value {
    let verification_kind = classify_verification_command(command);
    let changed_tests = baseline_tests
        .iter()
        .filter(|(path, hash)| {
            current_files
                .get(*path)
                .is_some_and(|current| current != *hash)
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let added_tests = current_files
        .keys()
        .filter(|path| is_test_source_path(path) && !baseline_tests.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    let deleted_tests = baseline_tests
        .keys()
        .filter(|path| !current_files.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    let observed_skips = observed_test_skips(output);
    let unexpected_skips = observed_skips > 0;
    let stale = checked_revision != current_revision;
    let passed = exit_code == Some(0)
        && !timed_out
        && !output_truncated
        && deleted_tests.is_empty()
        && !unexpected_skips
        && !stale
        && verification_kind.is_some();
    serde_json::json!({
        "command":command,
        "verification_kind":verification_kind,
        "exit_status":exit_code,
        "timed_out":timed_out,
        "checked_revision":checked_revision,
        "current_revision":current_revision,
        "environment_identity":environment_identity,
        "output_sha256":hex::encode(Sha256::digest(output.as_bytes())),
        "output_bytes":output.len(),
        "output_truncated":output_truncated,
        "changed_tests":changed_tests,
        "added_tests":added_tests,
        "deleted_tests":deleted_tests,
        "skip_count":{"observed":observed_skips,"expected":0,"unexpected":unexpected_skips},
        "passed":passed,
        "stale":stale
    })
}

fn classify_verification_command(command: &[String]) -> Option<&'static str> {
    let program = command.first()?.replace('\\', "/");
    let program = program.rsplit('/').next()?.to_ascii_lowercase();
    let args = command
        .iter()
        .skip(1)
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    match program.as_str() {
        "cargo" => {
            let action = args.first()?.as_str();
            if !args.iter().any(|argument| argument == "--locked") {
                return None;
            }
            match action {
                "test" => Some("test"),
                "build" | "check" | "clippy" => Some("build"),
                _ => None,
            }
        }
        "npm" | "pnpm" | "yarn" => {
            let action = match args.first()?.as_str() {
                "test" | "build" | "lint" => args[0].as_str(),
                "run" => match args.get(1)?.as_str() {
                    "test" | "build" | "lint" => args[1].as_str(),
                    _ => return None,
                },
                _ => return None,
            };
            Some(if action == "test" { "test" } else { "build" })
        }
        "pytest" | "py.test" => Some("test"),
        "go" => match args.first()?.as_str() {
            "test" => Some("test"),
            "build" => Some("build"),
            _ => None,
        },
        "make" | "gmake" => match args.first()?.as_str() {
            "test" | "check" => Some("test"),
            "build" | "all" => Some("build"),
            _ => None,
        },
        "python" | "python3" => {
            if args.windows(2).any(|window| window == ["-m", "pytest"]) {
                Some("test")
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(crate) fn verify_application_migrations(
    checkout: &FsPath,
    fixture_path: &str,
    migration_paths: &[String],
) -> Result<serde_json::Value, ApiError> {
    if migration_paths.is_empty() || migration_paths.len() > 64 {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_migration_list_invalid",
            message: "provide 1 to 64 migration paths".into(),
        });
    }
    let fixture = safe_relative_file(checkout, fixture_path)?;
    let fixture_sql = read_bounded_sql(&fixture)?;
    let fixture_sha256 = hash_file(&fixture)?;
    let mut total_sql_bytes = fixture_sql.len();
    let connection = rusqlite::Connection::open_in_memory().map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "workspace_migration_database_failed",
        message: error.to_string(),
    })?;
    connection
        .execute_batch(&fixture_sql)
        .map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_migration_fixture_failed",
            message: format!("fixture failed on a new disposable SQLite database: {error}"),
        })?;
    let before = sqlite_schema_snapshot(&connection)?;
    let destructive = regex::Regex::new(
        r"(?i)\b(?:DROP\s+(?:TABLE|COLUMN|INDEX)|DELETE\s+FROM|TRUNCATE\s+TABLE)\b",
    )
    .expect("static destructive SQL detector");
    let mut applied = Vec::new();
    let mut destructive_statements = Vec::new();
    for path in migration_paths {
        let file = safe_relative_file(checkout, path)?;
        let sql = read_bounded_sql(&file)?;
        total_sql_bytes = total_sql_bytes.saturating_add(sql.len());
        if total_sql_bytes > 8 * 1024 * 1024 {
            return Err(ApiError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "workspace_migration_input_too_large",
                message: "fixture and migrations exceed the 8 MiB total limit".into(),
            });
        }
        for matched in destructive.find_iter(&sql) {
            let start = sql[..matched.start()]
                .rfind(';')
                .map_or(0, |index| index + 1);
            let end = sql[matched.start()..]
                .find(';')
                .map_or(sql.len(), |offset| matched.start() + offset);
            destructive_statements.push(serde_json::json!({
                "path":path,
                "statement":sql[start..end].trim().chars().take(512).collect::<String>()
            }));
        }
        if let Err(error) = connection.execute_batch(&sql) {
            return Ok(serde_json::json!({
                "passed":false,
                "fixture_path":fixture_path,
                "fixture_sha256":fixture_sha256,
                "applied":applied,
                "failed_migration":path,
                "error":error.to_string(),
                "destructive_statements":destructive_statements,
                "rollback_limitations":["Only the disposable SQLite database is discarded; no down-migration or data restoration behavior is inferred from this run."]
            }));
        }
        applied.push(serde_json::json!({"path":path,"sha256":hash_file(&file)?}));
    }
    let after = sqlite_schema_snapshot(&connection)?;
    let before_names = before
        .iter()
        .filter_map(|item| item.get("name").and_then(serde_json::Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    let after_names = after
        .iter()
        .filter_map(|item| item.get("name").and_then(serde_json::Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    let added = after_names
        .difference(&before_names)
        .copied()
        .collect::<Vec<_>>();
    let removed = before_names
        .difference(&after_names)
        .copied()
        .collect::<Vec<_>>();
    let before_sql = before
        .iter()
        .filter_map(|object| {
            Some((
                object.get("name")?.as_str()?,
                object.get("sql")?.as_str().unwrap_or_default(),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let after_sql = after
        .iter()
        .filter_map(|object| {
            Some((
                object.get("name")?.as_str()?,
                object.get("sql")?.as_str().unwrap_or_default(),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let changed = before_names
        .intersection(&after_names)
        .filter(|name| before_sql.get(**name) != after_sql.get(**name))
        .copied()
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "passed":true,
        "database":"disposable-in-memory-sqlite",
        "fixture_path":fixture_path,
        "fixture_sha256":fixture_sha256,
        "applied":applied,
        "schema_before":before,
        "schema_after":after,
        "added_schema_objects":added,
        "removed_schema_objects":removed,
        "changed_schema_objects":changed,
        "destructive_statements":destructive_statements,
        "rollback_limitations":["The database is disposable. This verifies forward application only; it does not claim a rollback path or data restoration semantics."]
    }))
}

fn read_bounded_sql(path: &FsPath) -> Result<String, ApiError> {
    let metadata =
        std::fs::metadata(path).map_err(|error| io_error("workspace_sql_metadata", error))?;
    if metadata.len() > 2 * 1024 * 1024 {
        return Err(ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "workspace_sql_too_large",
            message: "SQL fixture or migration exceeds 2 MiB".into(),
        });
    }
    String::from_utf8(std::fs::read(path).map_err(|error| io_error("workspace_sql_read", error))?)
        .map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_sql_not_utf8",
            message: "SQL file must be UTF-8".into(),
        })
}

fn sqlite_schema_snapshot(
    connection: &rusqlite::Connection,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let mut statement = connection.prepare(
        "SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    ).map_err(|error| ApiError { status: StatusCode::INTERNAL_SERVER_ERROR, code: "workspace_schema_query_failed", message: error.to_string() })?;
    let rows = statement.query_map([], |row| Ok(serde_json::json!({
        "type":row.get::<_,String>(0)?,"name":row.get::<_,String>(1)?,"sql":row.get::<_,Option<String>>(2)?
    }))).map_err(|error| ApiError { status: StatusCode::INTERNAL_SERVER_ERROR, code: "workspace_schema_query_failed", message: error.to_string() })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "workspace_schema_query_failed",
            message: error.to_string(),
        })
}

fn hash_file(path: &FsPath) -> Result<String, ApiError> {
    let bytes = std::fs::read(path).map_err(|error| io_error("workspace_sql_hash", error))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

/// Copy only bounded, regular, non-secret workspace files into a disposable tool container mount.
pub(crate) fn create_tool_snapshot(source_root: &FsPath) -> Result<tempfile::TempDir, ApiError> {
    let source_root = std::fs::canonicalize(source_root)
        .map_err(|error| io_error("workspace_job_source", error))?;
    let files = scan_workspace(&source_root)?;
    let snapshot = tempfile::Builder::new()
        .prefix("execlaw-workspace-job-")
        .tempdir()
        .map_err(|error| io_error("workspace_job_snapshot", error))?;
    let snapshot_root = std::fs::canonicalize(snapshot.path())
        .map_err(|error| io_error("workspace_job_snapshot", error))?;
    for (relative, contents) in files {
        let relative_path = PathBuf::from(&relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || is_secret_path(&relative_path)
        {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                code: "workspace_job_snapshot_path_denied",
                message: "snapshot contains a secret or invalid relative path".into(),
            });
        }
        let target = snapshot_root.join(&relative_path);
        let parent = target.parent().ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "workspace_job_snapshot_path_denied",
            message: "snapshot file has no parent directory".into(),
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| io_error("workspace_job_snapshot_directory", error))?;
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|error| io_error("workspace_job_snapshot_directory", error))?;
        if !canonical_parent.starts_with(&snapshot_root) {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                code: "workspace_job_snapshot_escape",
                message: "snapshot path resolved outside its temporary root".into(),
            });
        }
        std::fs::write(&target, contents)
            .map_err(|error| io_error("workspace_job_snapshot_file", error))?;
    }
    Ok(snapshot)
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
pub(crate) fn has_multiple_links(_path: &FsPath, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() > 1
}

#[cfg(windows)]
pub(crate) fn has_multiple_links(path: &FsPath, _metadata: &std::fs::Metadata) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    winapi_util::file::information(file)
        .map(|information| information.number_of_links() != 1)
        .unwrap_or(true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::workspaces::WorkspaceApplyFile;

    #[test]
    fn verification_helpers_identify_test_changes_and_unexpected_skips() {
        assert!(is_test_source_path("crates/server/tests/send_test.rs"));
        assert!(is_test_source_path("web/src/__tests__/chat.spec.tsx"));
        assert!(!is_test_source_path("crates/server/src/chats.rs"));
        assert_eq!(observed_test_skips("2 ignored; 1 skipped"), 3);
        assert_eq!(observed_test_skips("all 14 tests passed"), 0);
    }

    #[test]
    fn executor_evidence_rejects_claims_without_exit_status_and_marks_stale_or_deleted_tests() {
        let command = vec!["cargo".into(), "test".into(), "--locked".into()];
        let baseline =
            std::collections::BTreeMap::from([("tests/required.rs".into(), "before".into())]);
        let current = std::collections::BTreeMap::new();
        let forged_text = workspace_execution_evidence(
            &command,
            Some(1),
            false,
            false,
            "all tests passed",
            "rev-a",
            "rev-a",
            "toolchain@sha256:image",
            &baseline,
            &current,
        );
        assert_eq!(forged_text["passed"], false);
        assert_eq!(forged_text["deleted_tests"][0], "tests/required.rs");
        let assertion_output = workspace_execution_evidence(
            &["echo".into(), "tests passed".into()],
            Some(0),
            false,
            false,
            "tests passed",
            "rev-a",
            "rev-a",
            "toolchain@sha256:image",
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        );
        assert_eq!(
            assertion_output["verification_kind"],
            serde_json::Value::Null
        );
        assert_eq!(assertion_output["passed"], false);

        let same =
            std::collections::BTreeMap::from([("tests/required.rs".into(), "before".into())]);
        let stale = workspace_execution_evidence(
            &command,
            Some(0),
            false,
            false,
            "ok",
            "rev-a",
            "rev-b",
            "toolchain@sha256:image",
            &baseline,
            &same,
        );
        assert_eq!(stale["stale"], true);
        assert_eq!(stale["passed"], false);

        let skipped = workspace_execution_evidence(
            &command,
            Some(0),
            false,
            false,
            "1 skipped",
            "rev-a",
            "rev-a",
            "toolchain@sha256:image",
            &baseline,
            &same,
        );
        assert_eq!(skipped["skip_count"]["unexpected"], true);
        assert_eq!(skipped["passed"], false);
    }

    #[test]
    fn application_migrations_run_only_on_disposable_fixture_database_and_report_destruction() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("fixture.sql"),
            "CREATE TABLE accounts(id INTEGER PRIMARY KEY, legacy TEXT); CREATE TABLE obsolete(id INTEGER); INSERT INTO accounts VALUES (1, 'kept');",
        ).unwrap();
        std::fs::write(
            workspace.path().join("001.sql"),
            "ALTER TABLE accounts ADD COLUMN active INTEGER NOT NULL DEFAULT 1; DROP TABLE obsolete;",
        ).unwrap();
        std::fs::write(
            workspace.path().join("002.sql"),
            "CREATE TABLE audit_events(id INTEGER PRIMARY KEY, account_id INTEGER);",
        )
        .unwrap();
        let result = verify_application_migrations(
            workspace.path(),
            "fixture.sql",
            &["001.sql".into(), "002.sql".into()],
        )
        .unwrap();
        assert_eq!(result["passed"], true);
        assert_eq!(result["database"], "disposable-in-memory-sqlite");
        assert_eq!(result["added_schema_objects"][0], "audit_events");
        assert!(
            !result["destructive_statements"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            result["rollback_limitations"][0]
                .as_str()
                .unwrap()
                .contains("forward application only")
        );
        assert!(
            verify_application_migrations(
                workspace.path(),
                "../production.db",
                &["001.sql".into()]
            )
            .is_err()
        );
    }

    #[test]
    fn source_symbol_extraction_covers_rust_types_and_script_functions() {
        let symbols =
            code_symbols("pub fn run() {}\nstruct State {}\nexport function start() {}\n");
        assert_eq!(
            symbols
                .iter()
                .map(|item| item.0.as_str())
                .collect::<Vec<_>>(),
            ["run", "State", "start"]
        );
        assert_eq!(symbols[1].1, "struct");
    }

    #[tokio::test]
    async fn code_index_tracks_revision_renames_deletions_and_exclusions() {
        let state = crate::routes::test_app_state();
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn old_name() {}\n").unwrap();
        std::fs::write(temp.path().join(".env"), "SECRET=must_not_index\n").unwrap();
        let canonical = std::fs::canonicalize(temp.path()).unwrap();
        let root = WorkspaceStore::new(&state.db)
            .register_root(&canonical.to_string_lossy(), "controller", 1)
            .unwrap();
        let indexed = index_workspace(
            State(state.clone()),
            workspace_controller(),
            Path(root.workspace_id.clone()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(indexed["fresh"], true);
        let symbol_count: i64 = state
            .db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM state_workspace_code_symbols WHERE workspace_id=?1",
                    [&root.workspace_id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(symbol_count, 1);
        let secret_files: i64 = state
            .db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM state_workspace_code_files WHERE workspace_id=?1 AND path='.env'",
                    [&root.workspace_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(secret_files, 0);
        std::fs::remove_file(temp.path().join("src/lib.rs")).unwrap();
        std::fs::write(
            temp.path().join("src/new.ts"),
            "export function renamed() {}\n",
        )
        .unwrap();
        let status = code_index_status(
            State(state.clone()),
            workspace_controller(),
            Path(root.workspace_id.clone()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(status["fresh"], false);
        let indexed = index_workspace(
            State(state.clone()),
            workspace_controller(),
            Path(root.workspace_id.clone()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(indexed["coverage"]["indexed"], 1);
        assert!(indexed["coverage"]["excluded"].as_u64().unwrap() >= 1);
        let names: Vec<String> = state
            .db
            .with_conn(|c| {
                let mut statement = c.prepare(
                    "SELECT symbol FROM state_workspace_code_symbols WHERE workspace_id=?1 ORDER BY symbol",
                )?;
                let rows = statement.query_map([&root.workspace_id], |row| row.get(0))?;
                Ok(rows.collect::<Result<Vec<String>, _>>()?)
            })
            .unwrap();
        assert_eq!(names, ["renamed"]);
        assert!(!names.iter().any(|name| name.contains("SECRET")));
    }

    fn workspace_controller() -> AuthedUser {
        AuthedUser {
            user_id: "workspace-controller".into(),
            session_id: None,
            username: "workspace-controller".into(),
            display_name: "Workspace Controller".into(),
            email: None,
            role: UserRole::Controller,
            last_login_at: None,
        }
    }

    #[tokio::test]
    async fn workspace_toolchain_config_requires_controller_and_digest_provenance() {
        let state = crate::routes::test_app_state();
        let digest_reference = format!("execlaw/workspace-toolchain@sha256:{}", "a".repeat(64));
        let languages = std::collections::BTreeMap::from([(
            "rust".to_owned(),
            vec!["rust-analyzer".to_owned()],
        )]);

        let denied = set_execution_config(
            State(state.clone()),
            AuthedUser {
                role: UserRole::Viewer,
                ..workspace_controller()
            },
            Json(WorkspaceExecutionConfigRequest {
                image_reference: digest_reference.clone(),
                language_servers: languages.clone(),
                approve_image_digest: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(denied.status, StatusCode::FORBIDDEN);

        let unverified = set_execution_config(
            State(state.clone()),
            workspace_controller(),
            Json(WorkspaceExecutionConfigRequest {
                image_reference: digest_reference.clone(),
                language_servers: languages.clone(),
                approve_image_digest: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(unverified.code, "workspace_toolchain_provenance_required");

        let saved = set_execution_config(
            State(state.clone()),
            workspace_controller(),
            Json(WorkspaceExecutionConfigRequest {
                image_reference: digest_reference,
                language_servers: languages.clone(),
                approve_image_digest: true,
            }),
        )
        .await
        .unwrap();
        assert_eq!(saved.0["language_servers"]["rust"][0], "rust-analyzer");
        assert_eq!(saved.0["approval"], "controller_digest_approval");

        let reopened = WorkspaceStore::new(&state.db).execution_config().unwrap();
        assert_eq!(reopened.language_servers, languages);
    }

    #[test]
    fn container_snapshot_omits_secrets_and_never_mutates_run_checkout() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("src")).unwrap();
        std::fs::write(source.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(source.path().join(".env"), "API_KEY=private\n").unwrap();
        std::fs::create_dir_all(source.path().join("target/debug")).unwrap();
        std::fs::write(source.path().join("target/debug/output"), "ignored\n").unwrap();

        let snapshot = create_tool_snapshot(source.path()).unwrap();
        assert!(snapshot.path().join("src/main.rs").is_file());
        assert!(!snapshot.path().join(".env").exists());
        assert!(!snapshot.path().join("target/debug/output").exists());
        std::fs::write(
            snapshot.path().join("src/main.rs"),
            "changed in container\n",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(source.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn patch_batch_preflights_all_hashes_before_writing_any_file() {
        let checkout = tempfile::tempdir().unwrap();
        std::fs::write(checkout.path().join("first.txt"), "first-old").unwrap();
        std::fs::write(checkout.path().join("second.txt"), "second-human-edit").unwrap();
        let edits = vec![
            WorkspaceFileEdit {
                path: "first.txt".into(),
                expected_sha256: Some(digest(b"first-old")),
                content: "first-new".into(),
            },
            WorkspaceFileEdit {
                path: "second.txt".into(),
                expected_sha256: Some(digest(b"second-original")),
                content: "second-new".into(),
            },
        ];

        let error = apply_checkout_patch(checkout.path(), &edits).unwrap_err();
        assert_eq!(error.code, "workspace_patch_conflict");
        assert_eq!(
            std::fs::read_to_string(checkout.path().join("first.txt")).unwrap(),
            "first-old",
            "a later file conflict must not leave the earlier file patched"
        );
        assert_eq!(
            std::fs::read_to_string(checkout.path().join("second.txt")).unwrap(),
            "second-human-edit"
        );
    }

    #[test]
    fn patch_batch_replay_is_idempotent_and_duplicate_paths_are_rejected() {
        let checkout = tempfile::tempdir().unwrap();
        std::fs::write(checkout.path().join("file.txt"), "before").unwrap();
        let edit = WorkspaceFileEdit {
            path: "file.txt".into(),
            expected_sha256: Some(digest(b"before")),
            content: "after".into(),
        };
        let first = apply_checkout_patch(checkout.path(), std::slice::from_ref(&edit)).unwrap();
        let replay = apply_checkout_patch(checkout.path(), std::slice::from_ref(&edit)).unwrap();
        assert_eq!(first, replay);
        assert_eq!(
            std::fs::read_to_string(checkout.path().join("file.txt")).unwrap(),
            "after"
        );

        let duplicate_error =
            apply_checkout_patch(checkout.path(), &[edit.clone(), edit]).unwrap_err();
        assert_eq!(duplicate_error.code, "workspace_patch_duplicate_path");
    }

    #[tokio::test]
    async fn patch_route_is_durable_idempotent_and_edits_only_the_run_checkout() {
        let state = crate::routes::test_app_state();
        let source_root = tempfile::tempdir().unwrap();
        let original = b"operator-owned source";
        std::fs::write(source_root.path().join("src.txt"), original).unwrap();
        let canonical_root = std::fs::canonicalize(source_root.path()).unwrap();
        let store = WorkspaceStore::new(&state.db);
        let root = store
            .register_root(&canonical_root.to_string_lossy(), "controller", 1)
            .unwrap();
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) VALUES ('workspace-patch-test','ControllerDM','idle','Controller','Text')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO state_events (conversation_id,seq,kind,payload,committed_at,actor) VALUES ('workspace-patch-test',1,'user_msg',X'00',1,'controller')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let run = execlaw_core::runs::RunStore::new(&state.db)
            .create_run(&execlaw_core::runs::NewRun {
                conversation_id: execlaw_core::ConversationId::from("workspace-patch-test"),
                parent_run_id: None,
                input_event_seq: execlaw_core::EventSeq(1),
                started_at: 1,
                deadline_at: None,
            })
            .unwrap();
        let checkout_root = state.data_dir.join("workspace-checkouts");
        std::fs::create_dir_all(&checkout_root).unwrap();
        let checkout = checkout_root.join("patch-test-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::copy(source_root.path().join("src.txt"), checkout.join("src.txt")).unwrap();
        let checkpoint = store
            .create_checkpoint(
                &root.workspace_id,
                &run,
                None,
                &[("src.txt".into(), original.to_vec())],
                2,
            )
            .unwrap();
        store
            .bind_run_checkout(
                &run,
                &root.workspace_id,
                &checkpoint.checkpoint_id,
                &std::fs::canonicalize(&checkout).unwrap().to_string_lossy(),
                2,
            )
            .unwrap();
        let call = |content: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("idempotency-key", "patch-1".parse().unwrap());
            patch_run_workspace(
                State(state.clone()),
                crate::auth_extract::AuthedUser {
                    user_id: "controller".into(),
                    session_id: None,
                    username: "controller".into(),
                    display_name: "Controller".into(),
                    email: None,
                    role: UserRole::Controller,
                    last_login_at: None,
                },
                Path(run.clone()),
                headers,
                Json(WorkspacePatchRequest {
                    edits: vec![WorkspaceFileEdit {
                        path: "src.txt".into(),
                        expected_sha256: Some(digest(original)),
                        content: content.into(),
                    }],
                }),
            )
        };
        let first = call("agent proposal").await.unwrap().0;
        let replayed = call("agent proposal").await.unwrap().0;
        assert_eq!(first, replayed);
        assert_eq!(
            std::fs::read(source_root.path().join("src.txt")).unwrap(),
            original,
            "patching a run checkout cannot modify the registered source root"
        );
        assert_eq!(
            std::fs::read(checkout.join("src.txt")).unwrap(),
            b"agent proposal"
        );
        let conflict = call("different proposal").await.unwrap_err();
        assert_eq!(conflict.status, StatusCode::CONFLICT);
    }

    fn change(path: &str, base: &[u8], proposed: &[u8]) -> WorkspaceApplyFile {
        WorkspaceApplyFile {
            path: path.into(),
            base_sha256: Some(digest(base)),
            proposed_sha256: Some(digest(proposed)),
            status: "pending".into(),
            error: None,
        }
    }

    #[tokio::test]
    async fn workspace_patch_route_replays_durable_receipts_and_leaves_source_root_untouched() {
        let state = crate::routes::test_app_state();
        let source = tempfile::tempdir().unwrap();
        let source_file = source.path().join("src.txt");
        std::fs::write(&source_file, b"controller source").unwrap();
        let workspace = WorkspaceStore::new(&state.db);
        let root = workspace
            .register_root(
                &std::fs::canonicalize(source.path())
                    .unwrap()
                    .to_string_lossy(),
                "controller",
                1,
            )
            .unwrap();
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) VALUES ('workspace-patch-test','ControllerDM','idle','Controller','Text')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO state_events (conversation_id,seq,kind,payload,committed_at,actor) VALUES ('workspace-patch-test',1,'user_msg',X'00',1,'controller')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let run = execlaw_core::runs::RunStore::new(&state.db)
            .create_run(&execlaw_core::runs::NewRun {
                conversation_id: execlaw_core::ConversationId::from("workspace-patch-test"),
                parent_run_id: None,
                input_event_seq: execlaw_core::EventSeq(1),
                started_at: 1,
                deadline_at: None,
            })
            .unwrap();
        let checkout_parent = state.data_dir.join("workspace-checkouts");
        let checkout = checkout_parent.join("patch-receipt-test");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::copy(&source_file, checkout.join("src.txt")).unwrap();
        let checkpoint = workspace
            .create_checkpoint(
                &root.workspace_id,
                &run,
                None,
                &[("src.txt".into(), b"controller source".to_vec())],
                2,
            )
            .unwrap();
        workspace
            .bind_run_checkout(
                &run,
                &root.workspace_id,
                &checkpoint.checkpoint_id,
                &std::fs::canonicalize(&checkout).unwrap().to_string_lossy(),
                2,
            )
            .unwrap();
        let invoke = |content: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("Idempotency-Key", "patch-request-1".parse().unwrap());
            patch_run_workspace(
                State(state.clone()),
                crate::auth_extract::AuthedUser {
                    user_id: "controller".into(),
                    session_id: None,
                    username: "controller".into(),
                    display_name: "Controller".into(),
                    email: None,
                    role: UserRole::Controller,
                    last_login_at: None,
                },
                Path(run.clone()),
                headers,
                Json(WorkspacePatchRequest {
                    edits: vec![WorkspaceFileEdit {
                        path: "src.txt".into(),
                        expected_sha256: Some(digest(b"controller source")),
                        content: content.to_owned(),
                    }],
                }),
            )
        };
        let first = invoke("run proposal").await.unwrap().0;
        let replay = invoke("run proposal").await.unwrap().0;
        assert_eq!(first, replay);
        assert_eq!(
            std::fs::read(checkout.join("src.txt")).unwrap(),
            b"run proposal"
        );
        assert_eq!(std::fs::read(&source_file).unwrap(), b"controller source");
        assert_eq!(
            invoke("changed intent").await.unwrap_err().status,
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn workspace_apply_is_retry_safe_and_never_overwrites_a_later_human_edit() {
        let temp = tempfile::tempdir().unwrap();
        let root_dir = temp.path().join("root");
        let checkout_dir = temp.path().join("checkout");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&checkout_dir).unwrap();
        std::fs::write(root_dir.join("src.rs"), b"base").unwrap();
        std::fs::write(checkout_dir.join("src.rs"), b"proposed").unwrap();
        let root = std::fs::canonicalize(root_dir).unwrap();
        let checkout = std::fs::canonicalize(checkout_dir).unwrap();
        let change = change("src.rs", b"base", b"proposed");

        apply_workspace_file(&root, &checkout, "apply-1", &change).unwrap();
        assert_eq!(std::fs::read(root.join("src.rs")).unwrap(), b"proposed");

        // Reopening the service after the replace repeats the same journaled
        // file operation; the proposed hash makes the operation idempotent.
        apply_workspace_file(&root, &checkout, "apply-1", &change).unwrap();
        std::fs::write(root.join("src.rs"), b"operator edit").unwrap();
        let error = apply_workspace_file(&root, &checkout, "apply-1", &change).unwrap_err();
        assert!(matches!(error, WorkspaceApplyError::Conflict(_)));
        assert_eq!(
            std::fs::read(root.join("src.rs")).unwrap(),
            b"operator edit",
            "a post-apply human edit must remain untouched"
        );
    }

    #[test]
    fn competing_run_preview_cannot_replace_an_already_applied_workspace_change() {
        let temp = tempfile::tempdir().unwrap();
        let root_dir = temp.path().join("root");
        let first_checkout_dir = temp.path().join("first");
        let second_checkout_dir = temp.path().join("second");
        for directory in [&root_dir, &first_checkout_dir, &second_checkout_dir] {
            std::fs::create_dir(directory).unwrap();
        }
        std::fs::write(root_dir.join("src.rs"), b"base").unwrap();
        std::fs::write(first_checkout_dir.join("src.rs"), b"run one").unwrap();
        std::fs::write(second_checkout_dir.join("src.rs"), b"run two").unwrap();
        let root = std::fs::canonicalize(root_dir).unwrap();
        let first_checkout = std::fs::canonicalize(first_checkout_dir).unwrap();
        let second_checkout = std::fs::canonicalize(second_checkout_dir).unwrap();

        let run_one = change("src.rs", b"base", b"run one");
        apply_workspace_file(&root, &first_checkout, "run-one-apply", &run_one).unwrap();
        let run_two = change("src.rs", b"base", b"run two");
        let error =
            apply_workspace_file(&root, &second_checkout, "run-two-apply", &run_two).unwrap_err();
        assert!(matches!(error, WorkspaceApplyError::Conflict(_)));
        assert_eq!(std::fs::read(root.join("src.rs")).unwrap(), b"run one");
    }

    #[test]
    fn workspace_targets_reject_traversal_secrets_and_hardlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        assert!(safe_target(&root, "../outside.txt", true).is_err());
        assert!(safe_target(&root, ".env", true).is_err());

        #[cfg(unix)]
        {
            std::fs::write(root.join("source.txt"), b"private").unwrap();
            std::fs::hard_link(root.join("source.txt"), root.join("linked.txt")).unwrap();
            assert!(safe_target(&root, "linked.txt", false).is_err());
            assert!(safe_relative_file(&root, "linked.txt").is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_targets_reject_symbolic_link_escapes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        assert!(safe_target(&root, "linked/secret.txt", false).is_err());
        assert!(safe_relative_file(&root, "linked/secret.txt").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn workspace_targets_reject_symlink_file_escapes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside.txt");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(&outside, b"private").unwrap();
        if let Err(error) = std::os::windows::fs::symlink_file(&outside, root.join("linked.txt")) {
            // ERROR_PRIVILEGE_NOT_HELD is expected when Developer Mode is off.
            if error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("could not create symlink fixture: {error}");
        }
        let root = std::fs::canonicalize(root).unwrap();
        assert!(safe_target(&root, "linked.txt", false).is_err());
        assert!(safe_relative_file(&root, "linked.txt").is_err());
    }

    #[test]
    fn owned_change_restore_uses_hash_preconditions_and_preserves_external_edits() {
        let temp = tempfile::tempdir().unwrap();
        let root_dir = temp.path().join("root");
        let checkout_dir = temp.path().join("checkout");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&checkout_dir).unwrap();
        std::fs::write(root_dir.join("src.rs"), b"run-owned change").unwrap();
        std::fs::write(checkout_dir.join("src.rs"), b"base").unwrap();
        let root = std::fs::canonicalize(root_dir).unwrap();
        let checkout = std::fs::canonicalize(checkout_dir).unwrap();
        let restore = change("src.rs", b"run-owned change", b"base");

        apply_workspace_file(&root, &checkout, "restore-1", &restore).unwrap();
        assert_eq!(std::fs::read(root.join("src.rs")).unwrap(), b"base");

        std::fs::write(root.join("src.rs"), b"operator edit").unwrap();
        let error = apply_workspace_file(&root, &checkout, "restore-2", &restore).unwrap_err();
        assert!(matches!(error, WorkspaceApplyError::Conflict(_)));
        assert_eq!(
            std::fs::read(root.join("src.rs")).unwrap(),
            b"operator edit"
        );
    }

    #[test]
    fn interrupted_replace_recovers_from_its_owned_backup() {
        let temp = tempfile::tempdir().unwrap();
        let root_dir = temp.path().join("root");
        let checkout_dir = temp.path().join("checkout");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&checkout_dir).unwrap();
        std::fs::write(checkout_dir.join("src.rs"), b"proposed").unwrap();
        let root = std::fs::canonicalize(root_dir).unwrap();
        let checkout = std::fs::canonicalize(checkout_dir).unwrap();
        let change = change("src.rs", b"base", b"proposed");
        let backup = root.join(format!(
            ".execlaw-apply-apply-2-{}.bak",
            hex::encode(Sha256::digest(b"src.rs"))[..16].to_owned()
        ));
        // This is the durable state after the original was renamed but before
        // the staged replacement was installed.
        std::fs::write(&backup, b"base").unwrap();

        apply_workspace_file(&root, &checkout, "apply-2", &change).unwrap();
        assert_eq!(std::fs::read(root.join("src.rs")).unwrap(), b"proposed");
        assert!(!backup.exists());
    }
}
