//! Deterministic, event-backed social archive projection.
//! SQLite is authoritative; Obsidian files are regenerable views.

use crate::state::AppState;
use execlaw_core::ids::ConversationId;
use execlaw_core::message_archive::{ArchiveConversation, ArchiveMessage, MessageArchiveStore};
use execlaw_core::principal::Principal;
use execlaw_script::InboundMessage;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub fn archive_inbound(
    state: &AppState,
    message: &InboundMessage,
    cid: &ConversationId,
    sender: &Principal,
) -> Result<(), String> {
    let remote_id = message.group_id.as_deref().unwrap_or(&message.native_id);
    let kind = if message.group_id.is_some() {
        "group"
    } else {
        "direct"
    };
    let archive_id = stable_id(&[&message.channel, remote_id]);
    let message_id = stable_id(&[
        &message.channel,
        remote_id,
        &message.native_id,
        &message.timestamp_ms.unwrap_or_default().to_string(),
        &message.text,
    ]);
    let occurred_at = message
        .timestamp_ms
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
        / 1000;
    let name = message.display_name.as_deref();
    let store = MessageArchiveStore::new(&state.db);
    store
        .upsert_conversation(
            &archive_id,
            &message.channel,
            remote_id,
            kind,
            message.group_name.as_deref().or(name),
            Some(cid.as_str()),
            occurred_at,
        )
        .map_err(|e| format!("archive conversation: {e}"))?;
    store
        .upsert_participant(&archive_id, sender.id.as_str(), name, occurred_at)
        .map_err(|e| format!("archive participant: {e}"))?;
    let inserted = store
        .append_message(&ArchiveMessage {
            archive_message_id: &message_id,
            archive_id: &archive_id,
            source_event_seq: None,
            source_event_kind: "transport_inbound",
            direction: "inbound",
            sender_id: Some(sender.id.as_str()),
            sender_name: name,
            body: &message.text,
            occurred_at,
            source_message_id: Some(&message_id),
            created_at: chrono::Utc::now().timestamp(),
            delivery_status: "delivered",
            reply_to_message_id: None,
        })
        .map_err(|e| format!("archive message: {e}"))?;
    if inserted {
        project(&store, &archive_id, &archive_root())?;
    }
    Ok(())
}

pub fn archive_outbound_generated(
    state: &AppState,
    cid: &ConversationId,
    channel: &str,
    remote_id: &str,
    is_group: bool,
    body: &str,
) -> Result<String, String> {
    let archive_id = stable_id(&[channel, remote_id]);
    let message_id = stable_id(&[channel, remote_id, cid.as_str(), body]);
    let now = chrono::Utc::now().timestamp();
    let store = MessageArchiveStore::new(&state.db);
    store
        .upsert_conversation(
            &archive_id,
            channel,
            remote_id,
            if is_group { "group" } else { "direct" },
            None,
            Some(cid.as_str()),
            now,
        )
        .map_err(|e| format!("archive outbound conversation: {e}"))?;
    store
        .append_message(&ArchiveMessage {
            archive_message_id: &message_id,
            archive_id: &archive_id,
            source_event_seq: None,
            source_event_kind: "model_turn",
            direction: "outbound",
            sender_id: Some("execlaw-agent"),
            sender_name: Some("execlaw"),
            body,
            occurred_at: now,
            source_message_id: Some(&message_id),
            created_at: now,
            delivery_status: "generated",
            reply_to_message_id: None,
        })
        .map_err(|e| format!("archive outbound message: {e}"))?;
    project(&store, &archive_id, &archive_root())?;
    Ok(message_id)
}

pub fn mark_outbound_status(
    state: &AppState,
    cid: &ConversationId,
    channel: &str,
    remote_id: &str,
    message_id: &str,
    status: &str,
) -> Result<(), String> {
    let store = MessageArchiveStore::new(&state.db);
    store
        .update_delivery_status(message_id, status)
        .map_err(|e| format!("update outbound archive status: {e}"))?;
    let _ = cid;
    let _ = channel;
    let _ = remote_id;
    if let Some(message) = store
        .get_conversation(&stable_id(&[channel, remote_id]))
        .map_err(|e| format!("read outbound archive conversation: {e}"))?
    {
        project(&store, &message.archive_id, &archive_root())?;
    }
    Ok(())
}

fn project(store: &MessageArchiveStore<'_>, archive_id: &str, root: &Path) -> Result<(), String> {
    let conversation = store
        .get_conversation(archive_id)
        .map_err(|e| format!("read archive conversation: {e}"))?
        .ok_or_else(|| "archive conversation disappeared".to_owned())?;
    let date = chrono::DateTime::from_timestamp(conversation.last_seen_at, 0)
        .ok_or_else(|| "invalid archive timestamp".to_owned())?;
    let year = date.format("%Y").to_string();
    let month = date.format("%m").to_string();
    let folder = root
        .join(slug(&conversation.channel))
        .join(&conversation.conversation_kind)
        .join(slug(&conversation.remote_id));
    fs::create_dir_all(folder.join(&year)).map_err(|e| format!("create archive directory: {e}"))?;
    let title = conversation
        .display_name
        .as_deref()
        .unwrap_or(&conversation.remote_id)
        .replace(['\r', '\n'], " ");
    let link = format!("{year}/{year}-{month}");
    let metadata = format!(
        "---\ntype: social-archive\narchive_id: {}\ntransport: {}\nconversation_kind: {}\nremote_id: {}\nconversation_id: {}\ntags:\n  - archive/social\n  - archive/{}\n  - archive/{}\n---\n\n# {title}\n\n- First seen: {}\n- Last seen: {}\n- Monthly archive: [[{link}]]\n",
        conversation.archive_id,
        conversation.channel,
        conversation.conversation_kind,
        conversation.remote_id,
        conversation.conversation_id.as_deref().unwrap_or("unknown"),
        conversation.channel,
        conversation.conversation_kind,
        conversation.first_seen_at,
        conversation.last_seen_at
    );
    fs::write(folder.join("_conversation.md"), metadata)
        .map_err(|e| format!("write conversation metadata: {e}"))?;
    let messages = store
        .list_messages_for_month(
            archive_id,
            year.parse().unwrap_or(1970),
            month.parse().unwrap_or(1),
        )
        .map_err(|e| format!("read archive month: {e}"))?;
    let mut page = format!(
        "---\ntype: social-archive-month\narchive_id: {}\ntransport: {}\nconversation_kind: {}\nperiod: {year}-{month}\ntags:\n  - archive/social\n  - archive/{}\n  - archive/{}\n---\n\n# {title} - {year}-{month}\n",
        conversation.archive_id,
        conversation.channel,
        conversation.conversation_kind,
        conversation.channel,
        conversation.conversation_kind
    );
    for message in messages {
        let when = chrono::DateTime::from_timestamp(message.occurred_at, 0)
            .map(|v| v.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "unknown time".to_owned());
        let speaker = message
            .sender_name
            .as_deref()
            .or(message.sender_id.as_deref())
            .unwrap_or("unknown")
            .replace(['\r', '\n'], " ");
        let status = if message.direction == "outbound" {
            format!("\n> **Outbound** ({})\n", message.delivery_status)
        } else {
            String::new()
        };
        page.push_str(&format!(
            "\n## {when} - {speaker}{status}\n\n{}\n",
            message.body
        ));
    }
    fs::write(folder.join(&year).join(format!("{year}-{month}.md")), page)
        .map_err(|e| format!("write monthly archive: {e}"))?;
    write_indexes(root, &conversation, &folder)
}

fn write_indexes(
    root: &Path,
    conversation: &ArchiveConversation,
    folder: &Path,
) -> Result<(), String> {
    let relative = folder
        .strip_prefix(root)
        .unwrap_or(folder)
        .to_string_lossy()
        .replace('\\', "/");
    let transport_root = root.join(slug(&conversation.channel));
    fs::create_dir_all(&transport_root).map_err(|e| format!("create archive indexes: {e}"))?;
    append_index(
        &root.join("archive-index.md"),
        "# Social archive\n\n",
        &format!(
            "- [[{relative}/_conversation]] #{} #{}\n",
            conversation.channel, conversation.conversation_kind
        ),
    )?;
    append_index(
        &transport_root.join("archive-index.md"),
        &format!("# {} archive\n\n", conversation.channel),
        &format!("- [[{relative}/_conversation]]\n"),
    )
}

fn append_index(path: &Path, header: &str, line: &str) -> Result<(), String> {
    let existing = fs::read_to_string(path).unwrap_or_else(|_| header.to_owned());
    if existing.contains(line.trim()) {
        return Ok(());
    }
    fs::write(path, format!("{existing}{line}")).map_err(|e| format!("write archive index: {e}"))
}

fn archive_root() -> PathBuf {
    PathBuf::from(".obsidian").join("archive")
}

fn stable_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("archive-{:x}", hasher.finalize())
}

fn slug(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let value: String = value.trim_matches('-').chars().take(96).collect();
    if value.is_empty() {
        "unknown".to_owned()
    } else {
        value
    }
}
