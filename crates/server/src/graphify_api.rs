//! Graphify graph browser API.
//!
//! Provides server-side paged + filtered access to `graphify-out/graph.json`
//! so the SPA can render large graphs without loading the full JSON blob at
//! once.

use crate::auth_extract::AuthedUser;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use utoipa::ToSchema;

#[derive(Debug, Deserialize)]
pub struct GraphPageQuery {
    pub node_offset: Option<usize>,
    pub node_limit: Option<usize>,
    pub edge_offset: Option<usize>,
    pub edge_limit: Option<usize>,
    pub q: Option<String>,
    pub file_contains: Option<String>,
    pub label_contains: Option<String>,
    pub community: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GraphNodeView {
    pub id: String,
    pub label: String,
    pub community: i64,
    pub file: Option<String>,
    pub source_location: Option<String>,
    pub file_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GraphEdgeView {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GraphPageResponse {
    pub source_path: String,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub filtered_nodes: usize,
    pub filtered_edges: usize,
    pub node_offset: usize,
    pub node_limit: usize,
    pub node_has_more: bool,
    pub edge_offset: usize,
    pub edge_limit: usize,
    pub edge_has_more: bool,
    pub nodes: Vec<GraphNodeView>,
    pub edges: Vec<GraphEdgeView>,
}

fn graph_path() -> PathBuf {
    if let Ok(p) = std::env::var("EXECLAW_GRAPHIFY_GRAPH_JSON") {
        return PathBuf::from(p);
    }
    PathBuf::from("graphify-out").join("graph.json")
}

fn read_graph_json() -> Result<(PathBuf, Value), String> {
    let p = graph_path();
    let data = std::fs::read_to_string(&p).map_err(|e| format!("read graph.json failed: {e}"))?;
    let v: Value =
        serde_json::from_str(&data).map_err(|e| format!("parse graph.json failed: {e}"))?;
    Ok((p, v))
}

fn node_from_value(v: &Value) -> Option<GraphNodeView> {
    let id = v.get("id")?.as_str()?.to_owned();
    let label = v
        .get("label")
        .and_then(|x| x.as_str())
        .unwrap_or(id.as_str())
        .to_owned();
    let community = v.get("community").and_then(|x| x.as_i64()).unwrap_or(-1);
    let file = v.get("file").and_then(|x| x.as_str()).map(str::to_owned);
    let source_location = v
        .get("source_location")
        .and_then(|x| x.as_str())
        .map(str::to_owned);
    let file_type = v
        .get("file_type")
        .and_then(|x| x.as_str())
        .map(str::to_owned);
    Some(GraphNodeView {
        id,
        label,
        community,
        file,
        source_location,
        file_type,
    })
}

fn edge_from_value(v: &Value) -> Option<GraphEdgeView> {
    let source = v
        .get("source")
        .or_else(|| v.get("from"))
        .and_then(|x| x.as_str())?
        .to_owned();
    let target = v
        .get("target")
        .or_else(|| v.get("to"))
        .and_then(|x| x.as_str())?
        .to_owned();
    Some(GraphEdgeView { source, target })
}

fn norm(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn node_matches(n: &GraphNodeView, q: &GraphPageQuery) -> bool {
    if let Some(c) = q.community {
        if n.community != c {
            return false;
        }
    }
    if let Some(ref f) = q.file_contains {
        let needle = norm(f);
        let hay = n.file.as_deref().unwrap_or("");
        if !norm(hay).contains(&needle) {
            return false;
        }
    }
    if let Some(ref l) = q.label_contains {
        let needle = norm(l);
        if !norm(&n.label).contains(&needle) {
            return false;
        }
    }
    if let Some(ref term) = q.q {
        let needle = norm(term);
        let hay = format!("{} {}", n.label, n.file.as_deref().unwrap_or(""));
        if !norm(&hay).contains(&needle) {
            return false;
        }
    }
    true
}

#[utoipa::path(
    get,
    path = "/api/admin/graphify/graph",
    params(
        ("node_offset" = Option<usize>, Query),
        ("node_limit" = Option<usize>, Query),
        ("edge_offset" = Option<usize>, Query),
        ("edge_limit" = Option<usize>, Query),
        ("q" = Option<String>, Query),
        ("file_contains" = Option<String>, Query),
        ("label_contains" = Option<String>, Query),
        ("community" = Option<i64>, Query)
    ),
    responses(
        (status = 200, description = "Paged graph slice", body = GraphPageResponse)
    ),
    security(("bearer_jwt" = [])),
    tag = "graphify"
)]
pub async fn graph_page_handler(
    State(_state): State<AppState>,
    _user: AuthedUser,
    Query(query): Query<GraphPageQuery>,
) -> Result<Json<GraphPageResponse>, (axum::http::StatusCode, Json<Value>)> {
    let (path, graph) = read_graph_json().map_err(|e| {
        (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": {"code": "graph_unavailable", "message": e}})),
        )
    })?;

    let raw_nodes = graph
        .get("nodes")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let raw_edges = graph
        .get("edges")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();

    let nodes: Vec<GraphNodeView> = raw_nodes.iter().filter_map(node_from_value).collect();
    let total_nodes = nodes.len();

    let filtered_nodes: Vec<GraphNodeView> = nodes
        .into_iter()
        .filter(|n| node_matches(n, &query))
        .collect();
    let filtered_node_ids: HashSet<String> = filtered_nodes.iter().map(|n| n.id.clone()).collect();

    let edges_all: Vec<GraphEdgeView> = raw_edges.iter().filter_map(edge_from_value).collect();
    let total_edges = edges_all.len();
    let filtered_edges: Vec<GraphEdgeView> = edges_all
        .into_iter()
        .filter(|e| filtered_node_ids.contains(&e.source) && filtered_node_ids.contains(&e.target))
        .collect();

    let node_offset = query.node_offset.unwrap_or(0);
    let node_limit = query.node_limit.unwrap_or(400).clamp(1, 5_000);
    let edge_offset = query.edge_offset.unwrap_or(0);
    let edge_limit = query.edge_limit.unwrap_or(1200).clamp(1, 10_000);

    let node_end = node_offset
        .saturating_add(node_limit)
        .min(filtered_nodes.len());
    let edge_end = edge_offset
        .saturating_add(edge_limit)
        .min(filtered_edges.len());

    let page_nodes = if node_offset >= filtered_nodes.len() {
        Vec::new()
    } else {
        filtered_nodes[node_offset..node_end].to_vec()
    };
    let page_edges = if edge_offset >= filtered_edges.len() {
        Vec::new()
    } else {
        filtered_edges[edge_offset..edge_end].to_vec()
    };

    Ok(Json(GraphPageResponse {
        source_path: path.to_string_lossy().to_string(),
        total_nodes,
        total_edges,
        filtered_nodes: filtered_nodes.len(),
        filtered_edges: filtered_edges.len(),
        node_offset,
        node_limit,
        node_has_more: node_end < filtered_nodes.len(),
        edge_offset,
        edge_limit,
        edge_has_more: edge_end < filtered_edges.len(),
        nodes: page_nodes,
        edges: page_edges,
    }))
}

pub fn graphify_api_router() -> Router<AppState> {
    Router::new().route("/api/admin/graphify/graph", get(graph_page_handler))
}
