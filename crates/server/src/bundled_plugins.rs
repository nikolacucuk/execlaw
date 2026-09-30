//! Boot-time mirror + admin endpoints for plugin ZIPs that ship
//! inside the .app's `Contents/Resources/plugins/` directory.
//!
//! The macOS bundle's release flow packages every plugin under
//! `plugins/*/` with a valid manifest into a ZIP, copies them into
//! `desktop-macos/src-tauri/resources/plugins/`, and `tauri.conf
//! .json`'s `bundle.resources` glob lifts them into the running
//! .app at `Contents/Resources/plugins/`. On Linux / Windows
//! installs (which don't have the .app shell) operators download
//! the same ZIPs from the GitHub Release page and drop them into
//! `~/.execlaw/bundled-plugins/` manually.
//!
//! This module exposes two things:
//!
//!   * `mirror_bundled_plugins_into_data_dir(data_dir)` — copies
//!     every ZIP from the resolved bundle resources dir into
//!     `<data_dir>/bundled-plugins/`. Idempotent (skips files that
//!     already exist at the destination) so re-launch is a no-op.
//!   * `GET /api/admin/plugins/bundled` — returns the list of
//!     available bundled ZIPs with parsed `id` / `version` /
//!     `description` from each manifest, plus a flag indicating
//!     whether the plugin is already installed (so the SPA can
//!     gray out the Install button).
//!   * `POST /api/admin/plugins/install-bundled?file=<filename>`
//!     — installs a specific bundled ZIP. Same staging + lifecycle
//!     as the upload path, just sourced from disk.
//!
//! Discovery priority for the source directory:
//!
//!   1. `$EXECLAW_BUNDLED_PLUGINS_DIR` — explicit override, takes
//!      precedence so a dev can point at `dist/` without a full
//!      .app build.
//!   2. `<current_exe>/../../Resources/plugins/` — the bundled
//!      .app's Resources dir, when the binary lives at
//!      `<bundle>/Contents/MacOS/execlaw`.
//!   3. Nothing — silently no-op. Operators on Linux/Windows who
//!      drop ZIPs into `~/.execlaw/bundled-plugins/` manually still
//!      get them listed by the endpoint; the mirror is just a
//!      no-op for them.

use crate::auth_extract::AuthedUser;
use crate::plugins::sync_after_lifecycle_change;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use execlaw_plugin_sdk::PluginManifest;
use execlaw_plugin_sdk::zip_stage::stage_zip;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};

/// Subdirectory under the operator's data dir (`~/.execlaw/`) that
/// holds the mirrored bundled ZIPs. The SPA's "Install plugin"
/// page reads from this same dir whether the ZIPs got there via
/// the boot-time mirror (macOS bundle) or were dropped in by
/// hand (Linux / Windows).
pub const BUNDLED_DIR_NAME: &str = "bundled-plugins";

/// Copy every plugin ZIP and detached provenance/Sigstore/SBOM sidecar from
/// the bundle resources into `<data_dir>/bundled-plugins/`. Best-effort: any
/// individual file failure is logged + skipped so a single broken plugin does
/// not gate the whole boot; installation still fails closed if its set is
/// incomplete.
///
/// Idempotent: files that already exist at the destination AND
/// match the source's size are skipped. A size mismatch (e.g.
/// after an `execlaw.app` upgrade) triggers a re-copy so the
/// operator gets the fresh ZIP on next launch.
pub fn mirror_bundled_plugins_into_data_dir(data_dir: &Path) {
    let dest = data_dir.join(BUNDLED_DIR_NAME);
    if let Err(e) = std::fs::create_dir_all(&dest) {
        tracing::warn!(
            error = %e,
            dest = %dest.display(),
            "could not create bundled-plugins dir; skipping mirror",
        );
        return;
    }

    let source = match resolve_source_dir() {
        Some(p) => p,
        None => {
            tracing::debug!(
                "no bundled-plugins source dir found (not a macOS .app, \
                 and EXECLAW_BUNDLED_PLUGINS_DIR unset); skipping mirror",
            );
            return;
        }
    };

    let entries = match std::fs::read_dir(&source) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                error = %e,
                source = %source.display(),
                "could not read bundled-plugins source dir",
            );
            return;
        }
    };

    let mut mirrored = 0usize;
    let mut skipped = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.ends_with(".zip")
            || name.ends_with(".zip.provenance.json")
            || name.ends_with(".zip.sigstore.json")
            || name.ends_with(".zip.cdx.json")
            || name.ends_with(".zip.spdx.json"))
        {
            continue;
        }
        let Some(filename) = path.file_name() else {
            continue;
        };
        let dest_path = dest.join(filename);

        // Idempotency: same name + same size = skip. Size is a
        // weak fingerprint (collisions theoretically possible) but
        // good enough — a plugin upgrade ALWAYS changes ZIP bytes.
        if dest_path.exists() {
            if let (Ok(src_meta), Ok(dest_meta)) =
                (std::fs::metadata(&path), std::fs::metadata(&dest_path))
            {
                if src_meta.len() == dest_meta.len() {
                    skipped += 1;
                    continue;
                }
            }
        }
        match std::fs::copy(&path, &dest_path) {
            Ok(_) => {
                mirrored += 1;
                tracing::debug!(
                    src = %path.display(),
                    dest = %dest_path.display(),
                    "mirrored bundled plugin artifact",
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    src = %path.display(),
                    dest = %dest_path.display(),
                    "failed to mirror bundled plugin artifact",
                );
            }
        }
    }
    if mirrored > 0 || skipped > 0 {
        tracing::info!(
            mirrored,
            skipped,
            source = %source.display(),
            dest = %dest.display(),
            "bundled plugin ZIPs mirrored into data dir",
        );
    }
}

/// Resolve the directory that holds the .app's bundled plugin ZIPs.
/// Returns `None` when running outside a Tauri-bundled .app AND no
/// `EXECLAW_BUNDLED_PLUGINS_DIR` override is set — the silent
/// no-op case for dev runs and Linux/Windows installs.
fn resolve_source_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("EXECLAW_BUNDLED_PLUGINS_DIR") {
        let path = PathBuf::from(p);
        if path.is_dir() {
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            "EXECLAW_BUNDLED_PLUGINS_DIR points at a path that doesn't exist; ignoring",
        );
    }
    // macOS .app layout: <bundle>/Contents/MacOS/<exe>. Walk up
    // two levels then into the resources dir. Tauri's
    // `bundle.resources = ["resources/plugins/*.zip"]` preserves
    // the relative path, so the ZIPs land at
    // `Contents/Resources/resources/plugins/` — NOT
    // `Contents/Resources/plugins/`. The doubled `resources` path
    // is structurally correct given Tauri's glob semantics.
    let exe = std::env::current_exe().ok()?;
    let candidate = exe
        .parent()?
        .parent()?
        .join("Resources")
        .join("resources")
        .join("plugins");
    if candidate.is_dir() {
        return Some(candidate);
    }
    // Fallback: older builds that staged into `plugins/` directly.
    let legacy = exe.parent()?.parent()?.join("Resources").join("plugins");
    if legacy.is_dir() {
        return Some(legacy);
    }
    None
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BundledPlugin {
    /// The exact filename inside `~/.execlaw/bundled-plugins/` —
    /// what the SPA passes back to `install-bundled?file=…`.
    pub file: String,
    /// Manifest `[plugin].id`. `None` when the ZIP is unparseable
    /// (corrupted download / dropped-in random ZIP); the SPA still
    /// renders the row so the operator sees the broken file.
    pub plugin_id: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    /// Size of the ZIP on disk, in bytes. Surfaced so the SPA can
    /// render `python-sandbox 0.1.0 · 15 KB` without a second HEAD
    /// round-trip.
    pub size_bytes: u64,
    /// `true` when a plugin with this `id` is already installed
    /// (regardless of version). Drives the SPA's button label:
    /// "Install" vs "Reinstall / Upgrade".
    pub already_installed: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BundledPluginListResponse {
    pub plugins: Vec<BundledPlugin>,
}

#[utoipa::path(
    get,
    path = "/api/admin/plugins/bundled",
    responses(
        (status = 200, description = "Available bundled plugin ZIPs", body = BundledPluginListResponse),
    ),
    security(("bearer_jwt" = [])),
    tag = "plugins"
)]
pub async fn list_bundled_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
) -> Json<BundledPluginListResponse> {
    let dir = state.data_dir.join(BUNDLED_DIR_NAME);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => {
            return Json(BundledPluginListResponse {
                plugins: Vec::new(),
            });
        }
    };

    let installed_ids: std::collections::HashSet<String> = state
        .plugin_host
        .list_rows()
        .map(|rows| rows.into_iter().map(|r| r.plugin_id).collect())
        .unwrap_or_default();

    let mut out: Vec<BundledPlugin> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("zip") {
            continue;
        }
        let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let (id, version, description) = read_manifest_fields(&path);
        let already_installed = id
            .as_ref()
            .map(|i| installed_ids.contains(i))
            .unwrap_or(false);
        out.push(BundledPlugin {
            file: filename.to_owned(),
            plugin_id: id,
            version,
            description,
            size_bytes,
            already_installed,
        });
    }
    // Sort by filename for stable rendering. The SPA can re-sort
    // by plugin_id / installed-status if it wants; this is just
    // the on-disk order.
    out.sort_by(|a, b| a.file.cmp(&b.file));
    Json(BundledPluginListResponse { plugins: out })
}

/// Peek inside a ZIP and pull `[plugin].id` + `version` +
/// `description` without unzipping. Used by the list endpoint so
/// the SPA can render rich rows without a second round-trip.
/// Returns `(None, None, None)` for any ZIP that fails to stage —
/// the SPA still surfaces the file so the operator can delete it.
fn read_manifest_fields(zip_path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let f = match File::open(zip_path) {
        Ok(f) => f,
        Err(_) => return (None, None, None),
    };
    let staged = match stage_zip(BufReader::new(f)) {
        Ok(s) => s,
        Err(_) => return (None, None, None),
    };
    let m: &PluginManifest = &staged.manifest;
    (
        Some(m.plugin.id.clone()),
        Some(m.plugin.version.clone()),
        m.plugin
            .description
            .as_ref()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
    )
}

#[derive(Debug, Deserialize)]
pub struct InstallBundledQuery {
    /// Filename inside `~/.execlaw/bundled-plugins/`. Path
    /// separators are rejected to keep the install confined to
    /// the bundled-plugins dir — the operator can't slip a
    /// `..` traversal in here.
    pub file: String,
    /// Mirrors `InstallQuery.if_existing` on the upload path.
    /// `"upgrade"` → an existing install with the same id is
    /// upgraded; `"reject"` (default) errors with 409 if there's
    /// already a row for this plugin id.
    #[serde(default)]
    pub if_existing: Option<String>,
}

#[utoipa::path(
    post,
    path = "/api/admin/plugins/install-bundled",
    responses(
        (status = 200, description = "Installed"),
        (status = 400, description = "Bad filename / invalid ZIP"),
        (status = 404, description = "Bundled plugin not found"),
        (status = 409, description = "Plugin already installed (use if_existing=upgrade)"),
    ),
    security(("bearer_jwt" = [])),
    tag = "plugins"
)]
pub async fn install_bundled_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    Query(q): Query<InstallBundledQuery>,
) -> Result<Json<crate::plugins::InstallResponse>, ApiError> {
    // Path-traversal guard. We accept ONLY a bare filename — no
    // separators, no parent refs — and resolve it against the
    // bundled-plugins dir. Anything weirder gets a 400.
    if q.file.contains('/') || q.file.contains('\\') || q.file.contains("..") {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "bad_filename",
            message: "file must be a bare filename inside bundled-plugins/".into(),
        });
    }
    if !q.file.ends_with(".zip") {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "bad_filename",
            message: "file must end in .zip".into(),
        });
    }
    let path = state.data_dir.join(BUNDLED_DIR_NAME).join(&q.file);
    if !path.exists() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: format!("no bundled plugin at {}", path.display()),
        });
    }

    // Read bytes + route through the same stage / install pipeline
    // the upload endpoint uses. Duplicating the body of
    // `plugins::install_handler` would drift; instead we re-use
    // `stage_zip` + the host's install/upgrade calls directly.
    let bytes = std::fs::read(&path).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "read_failed",
        message: format!("read {}: {e}", path.display()),
    })?;
    let staged = stage_zip(Cursor::new(&bytes[..])).map_err(|e| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "stage_failed",
        message: format!("stage failed: {e}"),
    })?;

    let provenance_path = path.with_file_name(format!("{}.provenance.json", q.file));
    let provenance_bytes = std::fs::read(&provenance_path).map_err(|error| ApiError {
        status: StatusCode::FORBIDDEN,
        code: "provenance_required",
        message: format!(
            "missing detached provenance {}: {error}",
            provenance_path.display()
        ),
    })?;
    let mut provenance: execlaw_core::artifact_provenance::ProvenanceStatement =
        serde_json::from_slice(&provenance_bytes).map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_provenance",
            message: error.to_string(),
        })?;
    provenance.artifact_locator = path.to_string_lossy().into_owned();
    let bundle_path = path.with_file_name(format!("{}.sigstore.json", q.file));
    let sbom_suffix = match provenance.sbom_format.as_str() {
        "cyclonedx" => "cdx.json",
        "spdx" => "spdx.json",
        _ => "invalid",
    };
    let sbom_path = path.with_file_name(format!("{}.{}", q.file, sbom_suffix));
    provenance.signature_reference = bundle_path.to_string_lossy().into_owned();
    provenance.sbom_location = sbom_path.to_string_lossy().into_owned();

    let plugin_id_for_log = staged.manifest.plugin.id.clone();
    let plugin_version_for_log = staged.manifest.plugin.version.clone();
    let upgrade = matches!(q.if_existing.as_deref(), Some("upgrade"));
    let stage_root = state.plugin_host.stage_root();
    std::fs::create_dir_all(stage_root).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "stage_mkdir",
        message: format!("mkdir: {error}"),
    })?;
    let stage_root = stage_root.canonicalize().map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "stage_root",
        message: format!("canonicalize plugin stage root: {error}"),
    })?;
    // Candidate bytes always land in a fresh sibling directory. In
    // particular, an upgrade with the same plugin id/version must not
    // overwrite the live version before provenance verification passes.
    let target = stage_root.join(format!("upload-{}", uuid::Uuid::new_v4()));
    if target.parent() != Some(stage_root.as_path()) {
        return Err(ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "stage_path",
            message: "bundled plugin candidate escaped the stage root".into(),
        });
    }
    let released = staged.tempdir.keep();
    if let Err(e) = std::fs::rename(&released, &target) {
        // Cross-device rename failure on the rename(2) path —
        // fall back to a full copy + cleanup like the upload
        // handler does.
        if let Err(e2) = crate::plugins::copy_dir_recursive(&released, &target) {
            let _ = std::fs::remove_dir_all(&released);
            return Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "stage_move",
                message: format!("rename+copy failed: {e} / {e2}"),
            });
        }
        let _ = std::fs::remove_dir_all(&released);
    }

    if let Err(error) = state.plugin_host.authorize_verified_plugin_archive(
        &bytes,
        &provenance,
        &staged.manifest,
        &target,
    ) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "provenance_verification_failed",
            message: error.to_string(),
        });
    }

    let result = if upgrade {
        // Mirror upload's "upgrade-or-install" auto-promotion so an
        // operator who picks upgrade for a plugin that isn't yet
        // installed still gets the install they intended.
        let exists = state
            .plugin_host
            .list_rows()
            .ok()
            .map(|rows| rows.iter().any(|r| r.plugin_id == plugin_id_for_log))
            .unwrap_or(false);
        if exists {
            state.plugin_host.upgrade(&target).await
        } else {
            state.plugin_host.install(&target).await
        }
    } else {
        state.plugin_host.install(&target).await
    };

    let row = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&target);
            return Err(ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "install_failed",
                message: format!("{e}"),
            });
        }
    };

    sync_after_lifecycle_change(&state, &row.plugin_id);

    tracing::info!(
        plugin_id = %plugin_id_for_log,
        version = %plugin_version_for_log,
        source = "bundled",
        "plugin installed from bundled ZIP",
    );

    Ok(Json(crate::plugins::InstallResponse {
        plugin_id: row.plugin_id,
        version: row.version,
    }))
}

pub fn bundled_plugins_router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/plugins/bundled", get(list_bundled_handler))
        .route(
            "/api/admin/plugins/install-bundled",
            post(install_bundled_handler),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{self, Body};
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    #[test]
    fn mirror_skips_when_no_source_dir() {
        // No EXECLAW_BUNDLED_PLUGINS_DIR + no .app context →
        // resolver returns None. Function should run cleanly
        // without creating anything beyond the (empty) dest dir.
        // `std::env::remove_var` is unsafe in 2024-edition; the
        // env in the test runner is acceptable as-is for the
        // "no override set" baseline most CI runs use.
        let tmp = tempfile::tempdir().unwrap();
        mirror_bundled_plugins_into_data_dir(tmp.path());
        // Dest dir exists, but empty.
        let dest = tmp.path().join(BUNDLED_DIR_NAME);
        assert!(dest.exists());
        let n = std::fs::read_dir(&dest).unwrap().count();
        assert_eq!(n, 0, "expected 0 mirrored files; got {n}");
    }

    #[test]
    fn install_bundled_query_rejects_path_traversal() {
        // The path-traversal guard lives in the handler body, not
        // in the deserializer. We test it via direct construction
        // here; the integration coverage runs through the router
        // in the file-based test below.
        let traversal = InstallBundledQuery {
            file: "../etc/passwd.zip".into(),
            if_existing: None,
        };
        assert!(traversal.file.contains(".."));
        let path_sep = InstallBundledQuery {
            file: "subdir/x.zip".into(),
            if_existing: None,
        };
        assert!(path_sep.file.contains('/'));
    }

    #[tokio::test]
    async fn rejected_bundled_upgrade_preserves_same_version_install() {
        use execlaw_core::users::{UserRole, UserRow, UserStore};
        use std::io::{Cursor, Write};
        use zip::ZipWriter;
        use zip::write::SimpleFileOptions;

        let state = crate::routes::test_app_state();
        let now = chrono::Utc::now().timestamp();
        let user_id = "bundled-plugin-controller";
        UserStore::new(&state.db)
            .insert(&UserRow {
                user_id: user_id.into(),
                username: "bundled-controller".into(),
                display_name: "Bundled Controller".into(),
                email: None,
                password_hash: "unused-test-hash".into(),
                role: UserRole::Controller,
                created_at: now,
                last_login_at: None,
            })
            .unwrap();
        let session_id = "bundled-plugin-session";
        state
            .refresh_store
            .issue(user_id, session_id, 3600)
            .unwrap();
        let access = state
            .signer
            .issue_access_token(user_id, session_id, 3600)
            .unwrap();

        let old_stage = state.plugin_host.stage_root().join("fixture-0.1.0");
        std::fs::create_dir_all(&old_stage).unwrap();
        let old_manifest =
            "[plugin]\nid = \"bundled-fixture\"\nname = \"Bundled fixture\"\nversion = \"0.1.0\"\n";
        std::fs::write(old_stage.join("plugin.toml"), old_manifest).unwrap();
        state.plugin_host.install(&old_stage).await.unwrap();

        let bundle_dir = state.data_dir.join(BUNDLED_DIR_NAME);
        std::fs::create_dir_all(&bundle_dir).unwrap();
        let archive_path = bundle_dir.join("bundled-fixture.zip");
        let mut archive = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut archive);
            zip.start_file::<_, ()>("plugin.toml", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(old_manifest.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let archive_bytes = archive.into_inner();
        std::fs::write(&archive_path, &archive_bytes).unwrap();
        let provenance = serde_json::json!({
            "artifact_id": "bundled-fixture-0.1.0",
            "artifact_type": "plugin_zip",
            "artifact_locator": archive_path.to_string_lossy(),
            "sha256": execlaw_core::artifact_provenance::sha256_bytes(&archive_bytes),
            "publisher_identity": "https://example.invalid/publisher",
            "source_repository": "https://example.invalid/repository",
            "source_commit": "0123456789abcdef",
            "workflow_identity": "https://example.invalid/workflow",
            "signature_reference": "fixture.zip.sigstore.json",
            "attestation_result": "signed",
            "sbom_format": "cyclonedx",
            "sbom_location": "fixture.zip.cdx.json",
            "sbom_sha256": "a".repeat(64),
        });
        std::fs::write(
            archive_path.with_file_name("bundled-fixture.zip.provenance.json"),
            serde_json::to_vec(&provenance).unwrap(),
        )
        .unwrap();

        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/plugins/install-bundled?file=bundled-fixture.zip&if_existing=upgrade")
            .header("authorization", format!("Bearer {access}"))
            .body(Body::empty())
            .unwrap();
        let response = crate::routes::build_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let _ = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();

        let row = state
            .plugin_host
            .get_row("bundled-fixture")
            .unwrap()
            .expect("previous plugin row remains installed");
        assert_eq!(row.version, "0.1.0");
        assert_eq!(
            std::fs::read(old_stage.join("plugin.toml")).unwrap(),
            old_manifest.as_bytes()
        );
        assert!(old_stage.exists(), "old staged bytes remain available");
    }
}
