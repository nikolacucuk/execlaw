use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use execlaw_core::db::DbError;
use execlaw_core::ids::{ConversationId, EventSeq};
use execlaw_core::users::UserRole;
use rusqlite::{OptionalExtension, params};
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
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn failed(error: impl std::fmt::Display) -> axum::response::Response {
    tracing::warn!(%error, "nexus organization request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "organization unavailable" })),
    )
        .into_response()
}

fn failed_database(error: DbError) -> axum::response::Response {
    if matches!(&error, DbError::Backpressure) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "database is busy; retry shortly",
                "code": "db_backpressure"
            })),
        )
            .into_response();
    }
    failed(error)
}

fn controller(user: &AuthedUser) -> Result<(), axum::response::Response> {
    if user.role == UserRole::Controller {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "controller only" })),
        )
            .into_response())
    }
}

fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

fn rebuild_verified_search_index(
    state: &AppState,
    conversation_id: &ConversationId,
) -> Result<(), DbError> {
    let log = event_log(state);
    let mut indexed_seq = state.db.with_conn(|conn| {
        Ok(conn.query_row(
            "SELECT indexed_seq FROM state_conversation_event_search_state WHERE conversation_id = ?1",
            params![conversation_id.as_str()],
            |row| row.get::<_, i64>(0),
        ).optional()?)
    })?.unwrap_or(0);
    let log_tail_seq = log.last_seq(conversation_id)?.0;
    let rebuild_from_start = indexed_seq > log_tail_seq;
    if rebuild_from_start {
        indexed_seq = 0;
    }
    // replay_since verifies the complete HMAC chain before returning this
    // suffix. The derived FTS rows are therefore extended only from verified
    // events; a missing watermark triggers a one-time backfill.
    let events = log.replay_since(conversation_id, EventSeq(indexed_seq))?;
    if events.is_empty() && indexed_seq == 0 {
        return Ok(());
    }
    let mut first_batch = true;
    for event_batch in events.chunks(128) {
        let is_first_batch = first_batch;
        state.db.transaction(|tx| {
            let stored_seq = tx
                .query_row(
                    "SELECT indexed_seq FROM state_conversation_event_search_state WHERE conversation_id = ?1",
                    params![conversation_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            if !rebuild_from_start && stored_seq < indexed_seq {
                return Err(DbError::Invariant(
                    "conversation search watermark moved backwards during indexing".to_owned(),
                ));
            }
            if (is_first_batch && rebuild_from_start) || stored_seq == 0 {
                tx.execute(
                    "DELETE FROM state_conversation_event_search WHERE conversation_id = ?1",
                    params![conversation_id.as_str()],
                )?;
            }
            let mut current_seq = if is_first_batch && rebuild_from_start {
                0
            } else {
                stored_seq
            };
            for event in event_batch {
                // Keep each write lock short so an approval or streaming
                // continuation can acquire the same SQLite connection while
                // a large verified history is being indexed.
                if event.seq.0 <= current_seq {
                    continue;
                }
                current_seq = event.seq.0;
                if event.actor.as_deref() == Some(SYSTEM_ORCHESTRATOR_ACTOR) {
                    continue;
                }
                let Some(text) = extract_text(event) else {
                    continue;
                };
                let source = if event.kind == execlaw_core::events::EventKind::ModelTurn {
                    "execlaw".to_owned()
                } else {
                    extract_channel_origin(event).unwrap_or_else(|| "web".to_owned())
                };
                tx.execute(
                    "INSERT INTO state_conversation_event_search \
                     (conversation_id, seq, source, committed_at, text) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        conversation_id.as_str(),
                        event.seq.0,
                        source,
                        event.committed_at,
                        text
                    ],
                )?;
            }
            let batch_last_seq = event_batch
                .last()
                .map(|event| event.seq.0)
                .unwrap_or(current_seq);
            tx.execute(
                "INSERT INTO state_conversation_event_search_state (conversation_id, indexed_seq, updated_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(conversation_id) DO UPDATE SET indexed_seq = excluded.indexed_seq, updated_at = excluded.updated_at",
                params![conversation_id.as_str(), current_seq.max(batch_last_seq), chrono::Utc::now().timestamp()],
            )?;
            Ok(())
        })?;
        first_batch = false;
    }
    Ok(())
}

pub async fn list_organization(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
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
        Ok((annotations, views)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "annotations": annotations, "views": views })),
        )
            .into_response(),
        Err(error) => failed(error),
    }
}

pub async fn save_annotation(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
    Json(annotation): Json<NexusAnnotation>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    let branch_id = annotation
        .branch_id
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if annotation.seq <= 0
        || branch_id.is_some_and(|value| value.len() > 120)
        || annotation.tags.len() > 8
        || annotation.links.len() > 16
        || annotation
            .tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.len() > 32)
        || annotation.links.iter().any(|link| {
            link.target_seq <= 0
                || link.target_seq == annotation.seq
                || !matches!(
                    link.relation.as_str(),
                    "replies_to" | "forwarded_from" | "mentions" | "generated_from"
                )
        })
    {
        return invalid("invalid annotation");
    }
    let cid = ConversationId::from(conversation_id.as_str());
    let events = match event_log(&state).replay_since(&cid, EventSeq(0)) {
        Ok(events) => events,
        Err(error) => return failed(error),
    };
    if !events.iter().any(|event| event.seq.0 == annotation.seq)
        || annotation
            .links
            .iter()
            .any(|link| !events.iter().any(|event| event.seq.0 == link.target_seq))
    {
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
    if let Err(response) = controller(&user) {
        return response;
    }
    let name = view.name.trim();
    if name.is_empty()
        || name.len() > 48
        || [
            &view.filters.source,
            &view.filters.tag,
            &view.filters.kind,
            &view.filters.query,
        ]
        .iter()
        .any(|value| value.as_ref().is_some_and(|text| text.len() > 128))
    {
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
    if let Err(response) = controller(&user) {
        return response;
    }
    match state.db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM config_nexus_views WHERE conversation_id = ?1 AND name = ?2",
            params![conversation_id, name],
        )?;
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
    if let Err(response) = controller(&user) {
        return response;
    }
    let needle = query.q.trim().to_lowercase();
    if needle.len() < 2 || needle.len() > 128 {
        return invalid("search text must contain 2 to 128 characters");
    }
    let cid = ConversationId::from(conversation_id.as_str());
    let Some(fts) = fts_query(&needle) else {
        return invalid("search text must contain searchable characters");
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let source = query.source;
    let before = query.before;
    let executor = state.db.clone();
    let result = executor
        .run_blocking(move || {
            let result: Result<(Vec<serde_json::Value>, bool), DbError> = (|| {
                rebuild_verified_search_index(&state, &cid)?;
                let mut matches = state.db.with_conn(|conn| {
                    let mut statement = conn.prepare(
                        "SELECT seq, text, source, committed_at \
                         FROM state_conversation_event_search \
                         WHERE state_conversation_event_search MATCH ?1 \
                           AND conversation_id = ?2 \
                           AND (?3 IS NULL OR source = ?3 COLLATE NOCASE) \
                           AND (?4 IS NULL OR seq < ?4) \
                         ORDER BY seq DESC LIMIT ?5",
                    )?;
                    Ok(statement
                        .query_map(
                            params![fts, cid.as_str(), source, before, (limit + 1) as i64],
                            |row| {
                                Ok(serde_json::json!({
                                    "seq": row.get::<_, i64>(0)?,
                                    "text": row.get::<_, String>(1)?
                                        .chars()
                                        .take(180)
                                        .collect::<String>(),
                                    "source": row.get::<_, String>(2)?,
                                    "committed_at": row.get::<_, i64>(3)?,
                                }))
                            },
                        )?
                        .collect::<Result<Vec<_>, _>>()?)
                })?;
                let has_more = matches.len() > limit;
                matches.truncate(limit);
                Ok((matches, has_more))
            })();
            result
        })
        .await;
    let (matches, has_more) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return failed(error),
        Err(error) => return failed_database(error),
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({ "matches": matches, "has_more": has_more })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_extract::AuthedUser;
    use crate::chats::types::UserMessagePayload;
    use crate::routes::test_app_state;
    use axum::body::to_bytes;
    use execlaw_core::events::{EventKind, EventLog, PendingEvent};
    use execlaw_core::users::UserRole;

    fn controller() -> AuthedUser {
        AuthedUser {
            user_id: "controller-1".into(),
            session_id: None,
            username: "controller".into(),
            display_name: "Controller".into(),
            email: None,
            role: UserRole::Controller,
            last_login_at: None,
        }
    }

    fn viewer() -> AuthedUser {
        AuthedUser {
            user_id: "viewer-1".into(),
            session_id: None,
            username: "viewer".into(),
            display_name: "Viewer".into(),
            email: None,
            role: UserRole::Viewer,
            last_login_at: None,
        }
    }

    #[tokio::test]
    async fn conversation_search_remains_controller_only() {
        let response = search_messages(
            State(test_app_state()),
            viewer(),
            Path("private-conversation".to_owned()),
            Query(SearchQuery {
                q: "private needle".into(),
                source: None,
                before: None,
                limit: None,
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    fn append_user_message(state: &AppState, conversation: &ConversationId, text: &str) {
        let event = PendingEvent::encode(
            EventKind::UserMsg,
            &UserMessagePayload {
                text: text.into(),
                sender_principal_id: None,
                channel_origin: None,
                transport_recipient: None,
                timezone: None,
                attachment_ids: Vec::new(),
                applied_skill_names: Vec::new(),
            },
            None,
        )
        .unwrap();
        EventLog::new(&state.db)
            .with_hmac_key((**state.event_log_hmac_key.as_ref().unwrap()).clone())
            .commit_turn(conversation, EventSeq(0), vec![event])
            .unwrap();
    }

    async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn search_is_conversation_scoped_and_fails_closed_on_hmac_tampering() {
        let state = test_app_state();
        let first = ConversationId::from("search-first");
        let second = ConversationId::from("search-second");
        append_user_message(&state, &first, "orchid ledger phrase");
        append_user_message(&state, &second, "orchid ledger phrase");

        let (second_status, second_results) = response_json(
            search_messages(
                State(state.clone()),
                controller(),
                Path(second.as_str().to_owned()),
                Query(SearchQuery {
                    q: "orchid ledger".into(),
                    source: None,
                    before: None,
                    limit: Some(10),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(second_results["matches"].as_array().unwrap().len(), 1);

        let (status, first_results) = response_json(
            search_messages(
                State(state.clone()),
                controller(),
                Path(first.as_str().to_owned()),
                Query(SearchQuery {
                    q: "orchid ledger".into(),
                    source: None,
                    before: None,
                    limit: Some(10),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first_results["matches"].as_array().unwrap().len(), 1);
        assert_eq!(first_results["matches"][0]["seq"], 1);

        let (_, filtered) = response_json(
            search_messages(
                State(state.clone()),
                controller(),
                Path(first.as_str().to_owned()),
                Query(SearchQuery {
                    q: "orchid ledger".into(),
                    source: Some("signal".into()),
                    before: None,
                    limit: Some(10),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert!(filtered["matches"].as_array().unwrap().is_empty());

        state
            .db
            .with_conn(|connection| {
                connection.execute(
                "UPDATE state_events SET payload = X'00' WHERE conversation_id = ?1 AND seq = 1",
                [first.as_str()],
            )?;
                Ok(())
            })
            .unwrap();
        let (status, _) = response_json(
            search_messages(
                State(state),
                controller(),
                Path(first.as_str().to_owned()),
                Query(SearchQuery {
                    q: "orchid ledger".into(),
                    source: None,
                    before: None,
                    limit: Some(10),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn verified_search_projection_appends_only_after_its_watermark() {
        let state = test_app_state();
        let cid = ConversationId::from("incremental-search");
        append_user_message(&state, &cid, "first searchable message");
        rebuild_verified_search_index(&state, &cid).unwrap();
        let indexed: i64 = state.db.with_conn(|conn| Ok(conn.query_row(
            "SELECT indexed_seq FROM state_conversation_event_search_state WHERE conversation_id = ?1",
            [cid.as_str()], |row| row.get(0))?)).unwrap();
        assert_eq!(indexed, 1);
        append_user_message(&state, &cid, "second searchable message");
        rebuild_verified_search_index(&state, &cid).unwrap();
        let rows: i64 = state
            .db
            .with_conn(|conn| {
                Ok(conn.query_row(
            "SELECT COUNT(*) FROM state_conversation_event_search WHERE conversation_id = ?1",
            [cid.as_str()], |row| row.get(0))?)
            })
            .unwrap();
        let indexed: i64 = state.db.with_conn(|conn| Ok(conn.query_row(
            "SELECT indexed_seq FROM state_conversation_event_search_state WHERE conversation_id = ?1",
            [cid.as_str()], |row| row.get(0))?)).unwrap();
        assert_eq!((rows, indexed), (2, 2));
    }
}
