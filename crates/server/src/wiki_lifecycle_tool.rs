//! Built-in `wiki_lifecycle` tool.
//!
//! Phase 1 surface for local wiki lifecycle operations backed by the
//! workspace `.obsidian/wiki/topics` tree.

use async_trait::async_trait;
use execlaw_core::tool::{ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome, ToolSource};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WikiLifecycleAction {
    Ingest,
    Compile,
    Query,
    Ll,
}

#[derive(Debug, Deserialize)]
struct WikiLifecycleArgs {
    action: WikiLifecycleAction,
    #[serde(default)]
    vault_root: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    top_k: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WikiEntry {
    id: String,
    topic: String,
    title: String,
    source: Option<String>,
    tags: Vec<String>,
    confidence: Option<f32>,
    created_at: i64,
    content: String,
}

pub struct WikiLifecycleTool {
    descriptor: ToolDescriptor,
}

impl WikiLifecycleTool {
    pub fn new() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "wiki_lifecycle".into(),
                description: "Manage local wiki lifecycle artifacts in .obsidian/wiki/topics (ingest, compile, query, ll).".into(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["ingest", "compile", "query", "ll"] },
                        "vault_root": { "type": "string" },
                        "topic": { "type": "string" },
                        "title": { "type": "string" },
                        "content": { "type": "string" },
                        "source": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                        "query": { "type": "string" },
                        "top_k": { "type": "integer", "minimum": 1, "maximum": 50 }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::Medium,
                capabilities: vec![],
                default_allowed_classes: vec![
                    "Controller".into(),
                    "Delegated".into(),
                    "KnownTrusted".into(),
                    "KnownLimited".into(),
                ],
                sensitive: false,
            },
        }
    }
}

#[async_trait]
impl ToolImpl for WikiLifecycleTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, _ctx: ToolCtx, args: Value) -> ToolOutcome {
        let parsed: WikiLifecycleArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("invalid_argument", e.to_string()),
        };

        let root = PathBuf::from(
            parsed
                .vault_root
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(".obsidian/wiki"),
        );

        match parsed.action {
            WikiLifecycleAction::Ingest => ingest_entry(&root, parsed),
            WikiLifecycleAction::Compile => compile_topic(&root, parsed.topic),
            WikiLifecycleAction::Query => {
                query_entries(&root, parsed.topic, parsed.query, parsed.top_k)
            }
            WikiLifecycleAction::Ll => lifecycle_summary(&root),
        }
    }
}

fn ingest_entry(root: &Path, args: WikiLifecycleArgs) -> ToolOutcome {
    let topic_raw = match args.topic.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(v) => v.trim(),
        None => {
            return ToolOutcome::err(
                "invalid_argument",
                "action=ingest requires non-empty `topic`",
            );
        }
    };
    let title = match args.title.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(v) => v.trim().to_owned(),
        None => {
            return ToolOutcome::err(
                "invalid_argument",
                "action=ingest requires non-empty `title`",
            );
        }
    };
    let content = match args.content.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(v) => v.trim().to_owned(),
        None => {
            return ToolOutcome::err(
                "invalid_argument",
                "action=ingest requires non-empty `content`",
            );
        }
    };

    let topic_slug = slugify(topic_raw);
    if topic_slug.is_empty() {
        return ToolOutcome::err(
            "invalid_argument",
            "topic must contain at least one alphanumeric character",
        );
    }

    let now = chrono::Utc::now().timestamp();
    let id = format!("{}-{}", now, chrono::Utc::now().timestamp_subsec_millis());
    let entry = WikiEntry {
        id: id.clone(),
        topic: topic_slug.clone(),
        title: title.clone(),
        source: args.source,
        tags: args
            .tags
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
        confidence: args.confidence,
        created_at: now,
        content: content.clone(),
    };

    let topic_dir = root.join("topics").join(&topic_slug);
    let entries_dir = topic_dir.join("entries");
    if let Err(e) = fs::create_dir_all(&entries_dir) {
        return ToolOutcome::err("io_error", format!("create entries dir: {e}"));
    }

    let filename_stem = format!("{}-{}", entry.id, slugify(&title));
    let json_path = entries_dir.join(format!("{filename_stem}.json"));
    let md_path = entries_dir.join(format!("{filename_stem}.md"));

    let body = match serde_json::to_vec_pretty(&entry) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::err("encoding_error", e.to_string()),
    };
    if let Err(e) = fs::write(&json_path, body) {
        return ToolOutcome::err("io_error", format!("write entry json: {e}"));
    }

    let mut markdown = String::new();
    markdown.push_str("---\n");
    markdown.push_str(&format!("id: {}\n", entry.id));
    markdown.push_str(&format!("topic: {}\n", entry.topic));
    markdown.push_str(&format!("title: {}\n", entry.title.replace('\n', " ")));
    markdown.push_str(&format!("created_at: {}\n", entry.created_at));
    if let Some(src) = &entry.source {
        markdown.push_str(&format!("source: {}\n", src.replace('\n', " ")));
    }
    if !entry.tags.is_empty() {
        markdown.push_str("tags:\n");
        for tag in &entry.tags {
            markdown.push_str(&format!("  - {}\n", tag.replace('\n', " ")));
        }
    }
    if let Some(c) = entry.confidence {
        markdown.push_str(&format!("confidence: {:.3}\n", c));
    }
    markdown.push_str("---\n\n");
    markdown.push_str("# ");
    markdown.push_str(&entry.title);
    markdown.push_str("\n\n");
    markdown.push_str(&entry.content);
    markdown.push('\n');
    if let Err(e) = fs::write(&md_path, markdown) {
        return ToolOutcome::err("io_error", format!("write entry markdown: {e}"));
    }

    ToolOutcome::ok(json!({
        "ok": true,
        "topic": topic_slug,
        "entry_id": entry.id,
        "json_path": normalize_sep(&json_path),
        "markdown_path": normalize_sep(&md_path),
    }))
}

fn compile_topic(root: &Path, topic: Option<String>) -> ToolOutcome {
    let topics_root = root.join("topics");
    let topic_filter = topic.map(|t| slugify(&t)).filter(|s| !s.is_empty());
    let mut compiled_topics = Vec::new();
    let mut total_entries = 0usize;

    let topic_dirs = match list_topic_dirs(&topics_root) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::err("io_error", e),
    };
    for (topic_name, topic_dir) in topic_dirs {
        if let Some(filter) = &topic_filter
            && &topic_name != filter
        {
            continue;
        }
        let entries = match read_entries(&topic_dir.join("entries")) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("io_error", e),
        };
        if entries.is_empty() {
            continue;
        }
        total_entries += entries.len();
        let (compiled_md, compiled_json) = build_compiled_payloads(&topic_name, &entries);
        let compiled_md_path = topic_dir.join("compiled.md");
        let compiled_json_path = topic_dir.join("compiled.json");
        if let Err(e) = fs::write(&compiled_md_path, compiled_md) {
            return ToolOutcome::err("io_error", format!("write compiled.md: {e}"));
        }
        if let Err(e) = fs::write(&compiled_json_path, compiled_json) {
            return ToolOutcome::err("io_error", format!("write compiled.json: {e}"));
        }
        compiled_topics.push(json!({
            "topic": topic_name,
            "entries": entries.len(),
            "compiled_markdown_path": normalize_sep(&compiled_md_path),
            "compiled_json_path": normalize_sep(&compiled_json_path),
        }));
    }

    ToolOutcome::ok(json!({
        "ok": true,
        "compiled_topics": compiled_topics,
        "total_entries": total_entries,
    }))
}

fn query_entries(
    root: &Path,
    topic: Option<String>,
    query: Option<String>,
    top_k: Option<usize>,
) -> ToolOutcome {
    let q = match query.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(v) => v.trim().to_lowercase(),
        None => {
            return ToolOutcome::err(
                "invalid_argument",
                "action=query requires non-empty `query`",
            );
        }
    };
    let k = top_k.unwrap_or(8).clamp(1, 50);
    let topics_root = root.join("topics");
    let topic_filter = topic.map(|t| slugify(&t)).filter(|s| !s.is_empty());

    let topic_dirs = match list_topic_dirs(&topics_root) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::err("io_error", e),
    };

    let mut hits = Vec::new();
    for (topic_name, topic_dir) in topic_dirs {
        if let Some(filter) = &topic_filter
            && &topic_name != filter
        {
            continue;
        }
        let entries = match read_entries(&topic_dir.join("entries")) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("io_error", e),
        };
        for e in entries {
            let haystack = format!("{}\n{}", e.title.to_lowercase(), e.content.to_lowercase());
            if let Some(score) = lexical_score(&haystack, &q)
                && score > 0
            {
                hits.push((
                    score,
                    json!({
                        "topic": topic_name,
                        "entry_id": e.id,
                        "title": e.title,
                        "source": e.source,
                        "confidence": e.confidence,
                        "created_at": e.created_at,
                        "excerpt": excerpt_for(&e.content, 280),
                    }),
                ));
            }
        }
    }

    hits.sort_by(|a, b| b.0.cmp(&a.0));
    let results: Vec<Value> = hits.into_iter().take(k).map(|(_, v)| v).collect();

    ToolOutcome::ok(json!({
        "ok": true,
        "query": q,
        "count": results.len(),
        "results": results,
    }))
}

fn lifecycle_summary(root: &Path) -> ToolOutcome {
    let topics_root = root.join("topics");
    let topic_dirs = match list_topic_dirs(&topics_root) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::err("io_error", e),
    };

    let mut topics = Vec::new();
    let mut total_entries = 0usize;
    let now = chrono::Utc::now().timestamp();

    for (topic_name, topic_dir) in topic_dirs {
        let entries = match read_entries(&topic_dir.join("entries")) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("io_error", e),
        };
        if entries.is_empty() {
            continue;
        }
        total_entries += entries.len();
        let newest = entries.iter().map(|e| e.created_at).max().unwrap_or(0);
        let stale_days = if newest > 0 {
            ((now - newest).max(0) / 86_400) as i64
        } else {
            0
        };
        topics.push(json!({
            "topic": topic_name,
            "entries": entries.len(),
            "last_updated_at": newest,
            "stale_days": stale_days,
        }));
    }

    ToolOutcome::ok(json!({
        "ok": true,
        "topics": topics,
        "topic_count": topics.len(),
        "entry_count": total_entries,
        "root": normalize_sep(root),
    }))
}

fn list_topic_dirs(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let iter = fs::read_dir(root).map_err(|e| format!("read topics dir: {e}"))?;
    let mut out = Vec::new();
    for ent in iter {
        let ent = ent.map_err(|e| format!("read topics dir entry: {e}"))?;
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().to_string();
        out.push((name, path));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn read_entries(entries_dir: &Path) -> Result<Vec<WikiEntry>, String> {
    if !entries_dir.exists() {
        return Ok(Vec::new());
    }
    let iter = fs::read_dir(entries_dir).map_err(|e| format!("read entries dir: {e}"))?;
    let mut out = Vec::new();
    for ent in iter {
        let ent = ent.map_err(|e| format!("read entry file: {e}"))?;
        let path = ent.path();
        let ext = path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        if ext != "json" {
            continue;
        }
        let body = fs::read(&path).map_err(|e| format!("read entry json: {e}"))?;
        let parsed: WikiEntry = serde_json::from_slice(&body)
            .map_err(|e| format!("parse entry json {}: {e}", normalize_sep(&path)))?;
        out.push(parsed);
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

fn build_compiled_payloads(topic: &str, entries: &[WikiEntry]) -> (String, Vec<u8>) {
    let mut md = String::new();
    md.push_str("# ");
    md.push_str(topic);
    md.push_str("\n\n");
    md.push_str("## Summary\n\n");
    md.push_str(&format!("Total entries: {}\n\n", entries.len()));

    for entry in entries {
        md.push_str("## ");
        md.push_str(&entry.title);
        md.push_str("\n\n");
        md.push_str(&format!("- id: {}\n", entry.id));
        md.push_str(&format!("- created_at: {}\n", entry.created_at));
        if let Some(src) = &entry.source {
            md.push_str(&format!("- source: {}\n", src));
        }
        if !entry.tags.is_empty() {
            md.push_str(&format!("- tags: {}\n", entry.tags.join(", ")));
        }
        if let Some(c) = entry.confidence {
            md.push_str(&format!("- confidence: {:.3}\n", c));
        }
        md.push_str("\n");
        md.push_str(&entry.content);
        md.push_str("\n\n");
    }

    let compiled_json = serde_json::to_vec_pretty(&json!({
        "topic": topic,
        "entry_count": entries.len(),
        "generated_at": chrono::Utc::now().timestamp(),
        "entries": entries,
    }))
    .unwrap_or_else(|_| b"{}".to_vec());

    (md, compiled_json)
}

fn lexical_score(haystack: &str, query: &str) -> Option<i64> {
    let mut count = 0i64;
    for token in query.split_whitespace() {
        if token.is_empty() {
            continue;
        }
        let token_hits = haystack.matches(token).count() as i64;
        if token_hits == 0 {
            return None;
        }
        count += token_hits;
    }
    Some(count)
}

fn excerpt_for(s: &str, max: usize) -> String {
    let compact = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= max {
        compact
    } else {
        format!("{}...", &compact[..max])
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

fn normalize_sep(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub fn wiki_lifecycle_tools() -> Vec<Arc<dyn ToolImpl>> {
    vec![Arc::new(WikiLifecycleTool::new())]
}
