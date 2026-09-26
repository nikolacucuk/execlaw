use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use execlaw_core::db::DbError;
use execlaw_core::ids::{ConversationId, EventSeq};
use execlaw_core::users::UserRole;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::auth_extract::AuthedUser;
use crate::state::AppState;

use super::{SYSTEM_ORCHESTRATOR_ACTOR, event_log, extract_channel_origin, extract_text};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NexusLink {
    pub target_seq: i64,
    pub relation: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NexusAnnotation {
    pub seq: i64,
    pub branch_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<NexusLink>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NexusView {
    pub name: String,
    pub filters: NexusFilters,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NexusFilters {
    pub source: Option<String>,
    pub tag: Option<String>,
    pub kind: Option<String>,
    pub query: Option<String>,
}

fn invalid(message: &str) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": message }))).into_response()
}

fn failed(error: impl std::fmt::Display) -> axum::response::Response {
    tracing::warn!(%error, "nexus organization request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "organization unavailable" }))).into_response()
}

fn controller(user: &AuthedUser) -> Result<(), axum::response::Response> {
    if user.role == UserRole::Controller { Ok(()) } else {
        Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "controller only" }))).into_response())
    }
}

pub async fn list_organization(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) { return response; }
    let result = state.db.with_conn(|conn| {
        let mut annotations = Vec::new();
        let mut statement = conn.prepare("SELECT seq, branch_id FROM state_nexus_annotations WHERE conversation_id = ?1 ORDER BY seq")?;
        let rows = statement.query_map([&conversation_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)))?;
        for row in rows {
            let (seq, branch_id) = row?;
            let mut tags = Vec::new();
            let mut tag_statement = conn.prepare("SELECT tag FROM state_nexus_tags WHERE conversation_id = ?1 AND seq = ?2 ORDER BY tag")?;
            for tag in tag_statement.query_map(params![conversation_id, seq], |item| item.get(0))? {
                tags.push(tag?);
            }
            let mut links = Vec::new();
            let mut link_statement = conn.prepare("SELECT target_seq, relation FROM state_nexus_links WHERE conversation_id = ?1 AND seq = ?2 ORDER BY target_seq")?;
            for link in link_statement.query_map(params![conversation_id, seq], |item| Ok(NexusLink { target_seq: item.get(0)?, relation: item.get(1)? }))? {
                links.push(link?);
            }
            annotations.push(NexusAnnotation { seq, branch_id, tags, links });
        }
        let mut views = Vec::new();
        let mut statement = conn.prepare("SELECT name, filters_json FROM config_nexus_views WHERE conversation_id = ?1 ORDER BY name")?;
        for row in statement.query_map([&conversation_id], |item| Ok((item.get::<_, String>(0)?, item.get::<_, String>(1)?)))? {
            let (name, json) = row?;
            let filters = serde_json::from_str(&json).map_err(|error| DbError::Serde(error.to_string()))?;
            views.push(NexusView { name, filters });
        }
        Ok((annotations, views))
    });
    match result {
        Ok((annotations, views)) => (StatusCode::OK, Json(serde_json::json!({ "annotations": annotations, "views": views }))).into_response(),
        Err(error) => failed(error),
    }
}

pub async fn save_annotation(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
    Json(annotation): Json<NexusAnnotation>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) { return response; }
    let branch_id = annotation.branch_id.as_ref().map(|value| value.trim()).filter(|value| !value.is_empty());
    if annotation.seq <= 0 || branch_id.is_some_and(|value| value.len() > 120)
        || annotation.tags.len() > 8 || annotation.links.len() > 16
        || annotation.tags.iter().any(|tag| tag.trim().is_empty() || tag.len() > 32)
        || annotation.links.iter().any(|link| link.target_seq <= 0 || link.target_seq == annotation.seq
            || !matches!(link.relation.as_str(), "replies_to" | "forwarded_from" | "mentions" | "generated_from")) {
        return invalid("invalid annotation");
    }
    let cid = ConversationId::from(conversation_id.as_str());
    let events = match event_log(&state).replay_since(&cid, EventSeq(0)) {
        Ok(events) => events,
        Err(error) => return failed(error),
    };
    if !events.iter().any(|event| event.seq.0 == annotation.seq)
        || annotation.links.iter().any(|link| !events.iter().any(|event| event.seq.0 == link.target_seq)) {
        return invalid("message or relationship target not found in this conversation");
    }
    let result = state.db.transaction(|tx| {
        tx.execute("INSERT INTO state_nexus_annotations (conversation_id, seq, branch_id) VALUES (?1, ?2, ?3) ON CONFLICT(conversation_id, seq) DO UPDATE SET branch_id = excluded.branch_id",
            params![conversation_id, annotation.seq, branch_id])?;
        tx.execute("DELETE FROM state_nexus_tags WHERE conversation_id = ?1 AND seq = ?2", params![conversation_id, annotation.seq])?;
        tx.execute("DELETE FROM state_nexus_links WHERE conversation_id = ?1 AND seq = ?2", params![conversation_id, annotation.seq])?;
        for tag in &annotation.tags {
            tx.execute("INSERT OR IGNORE INTO state_nexus_tags (conversation_id, seq, tag) VALUES (?1, ?2, ?3)", params![conversation_id, annotation.seq, tag.trim()])?;
        }
        for link in &annotation.links {
            tx.execute("INSERT OR IGNORE INTO state_nexus_links (conversation_id, seq, target_seq, relation) VALUES (?1, ?2, ?3, ?4)",
                params![conversation_id, annotation.seq, link.target_seq, link.relation])?;
        }
        Ok(())
    });
    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "saved": true }))).into_response(),
        Err(error) => failed(error),
    }
}

pub async fn save_view(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
    Json(view): Json<NexusView>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) { return response; }
    let name = view.name.trim();
    if name.is_empty() || name.len() > 48 ||
        [&view.filters.source, &view.filters.tag, &view.filters.kind, &view.filters.query]
            .iter().any(|value| value.as_ref().is_some_and(|text| text.len() > 128)) {
        return invalid("invalid saved view");
    }
    let json = match serde_json::to_string(&view.filters) {
        Ok(json) => json,
        Err(error) => return failed(error),
    };
    match state.db.with_conn(|conn| {
        conn.execute("INSERT INTO config_nexus_views (conversation_id, name, filters_json) VALUES (?1, ?2, ?3) ON CONFLICT(conversation_id, name) DO UPDATE SET filters_json = excluded.filters_json",
            params![conversation_id, name, json])?;
        Ok(())
    }) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "saved": true }))).into_response(),
        Err(error) => failed(error),
    }
}

pub async fn delete_view(
    State(state): State<AppState>,
    user: AuthedUser,
    Path((conversation_id, name)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) { return response; }
    match state.db.with_conn(|conn| {
        conn.execute("DELETE FROM config_nexus_views WHERE conversation_id = ?1 AND name = ?2", params![conversation_id, name])?;
        Ok(())
    }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failed(error),
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub source: Option<String>,
    pub before: Option<i64>,
    pub limit: Option<usize>,
}

pub async fn search_messages(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) { return response; }
    let needle = query.q.trim().to_lowercase();
    if needle.len() < 2 || needle.len() > 128 {
        return invalid("search text must contain 2 to 128 characters");
    }
    let cid = ConversationId::from(conversation_id.as_str());
    let events = match event_log(&state).replay_since(&cid, EventSeq(0)) {
        Ok(events) => events,
        Err(error) => return failed(error),
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let mut matches = events.into_iter().rev().filter_map(|event| {
        if query.before.is_some_and(|before| event.seq.0 >= before) { return None; }
        if event.actor.as_deref() == Some(SYSTEM_ORCHESTRATOR_ACTOR) { return None; }
        let text = extract_text(&event)?;
        if !text.to_lowercase().contains(&needle) { return None; }
        let source = if event.kind == execlaw_core::events::EventKind::ModelTurn {
            "execlaw".to_owned()
        } else {
            extract_channel_origin(&event).unwrap_or_else(|| "web".to_owned())
        };
        if query.source.as_deref().is_some_and(|requested| !requested.eq_ignore_ascii_case(&source)) { return None; }
        Some(serde_json::json!({ "seq": event.seq.0, "text": text.chars().take(180).collect::<String>(),
            "source": source, "committed_at": event.committed_at }))
    }).take(limit + 1).collect::<Vec<_>>();
    let has_more = matches.len() > limit;
    matches.truncate(limit);
    (StatusCode::OK, Json(serde_json::json!({ "matches": matches, "has_more": has_more }))).into_response()
}