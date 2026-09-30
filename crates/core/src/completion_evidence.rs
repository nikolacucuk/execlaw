//! Proof checks for task completion evidence.

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::io::Read;

fn has_named_reference(value: &serde_json::Value, key: &str, expected: &str, depth: u8) -> bool {
    if depth == 0 {
        return false;
    }
    match value {
        serde_json::Value::Object(fields) => {
            fields.get(key).and_then(serde_json::Value::as_str) == Some(expected)
                || fields
                    .values()
                    .any(|value| has_named_reference(value, key, expected, depth - 1))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| has_named_reference(value, key, expected, depth - 1)),
        _ => false,
    }
}

fn artifact_has_run_producer(
    connection: &Connection,
    run_id: &str,
    artifact_id: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    let row: Option<(
        String,
        String,
        i64,
        String,
        Option<String>,
        String,
        i64,
        i64,
    )> = connection
        .query_row(
            "SELECT a.path,a.sha256,COALESCE(a.bytes,-1),a.kind,a.research_job_id,
                    r.conversation_id,r.input_event_seq,r.started_at
             FROM state_artifacts a JOIN state_runs r ON r.run_id=?2
             WHERE a.id=?1 AND a.kind IN ('plugin_artifact','research_pdf')
               AND a.created_at>=r.started_at
               AND (a.expires_at IS NULL OR a.expires_at>?3)",
            params![artifact_id, run_id, now],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    let row = if row.is_some() {
        row
    } else {
        connection
            .query_row(
                "SELECT a.path,a.sha256,j.id,r.conversation_id,r.input_event_seq,r.started_at
                 FROM state_research_jobs j
                 JOIN state_attachments a ON a.id=j.attachment_id
                   AND a.conversation_id=j.conversation_id
                 JOIN state_runs r ON r.run_id=?2 AND r.conversation_id=j.conversation_id
                 WHERE j.attachment_id=?1 AND j.status='complete'
                   AND j.created_at>=r.started_at AND a.received_at>=r.started_at",
                params![artifact_id, run_id],
                |row| {
                    let path: String = row.get(0)?;
                    let bytes = std::fs::metadata(&path)
                        .ok()
                        .and_then(|metadata| i64::try_from(metadata.len()).ok())
                        .unwrap_or(-1);
                    Ok((
                        path,
                        row.get(1)?,
                        bytes,
                        "research_pdf".to_owned(),
                        Some(row.get(2)?),
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?
    };
    let Some((path, digest, bytes, kind, research_job_id, conversation_id, input_seq, _)) = row
    else {
        return Ok(false);
    };
    if !file_matches(&path, &digest, bytes) {
        return Ok(false);
    }
    let research_job_is_complete = if kind == "research_pdf" {
        let Some(job_id) = research_job_id.as_deref() else {
            return Ok(false);
        };
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM state_research_jobs
              WHERE id=?1 AND conversation_id=?2 AND status='complete' AND attachment_id=?3)",
            params![job_id, conversation_id, artifact_id],
            |row| row.get::<_, bool>(0),
        )?
    } else {
        true
    };
    if !research_job_is_complete {
        return Ok(false);
    }
    let next_input: Option<i64> = connection.query_row(
        "SELECT MIN(input_event_seq) FROM state_runs
         WHERE conversation_id=?1 AND input_event_seq>?2",
        params![conversation_id, input_seq],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare_cached(
        "SELECT kind,payload FROM state_events
         WHERE conversation_id=?1 AND seq>=?2 AND seq<?3
           AND kind IN ('tool_result','card_closed') ORDER BY seq",
    )?;
    let events = statement.query_map(
        params![conversation_id, input_seq, next_input.unwrap_or(i64::MAX)],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )?;
    for event in events {
        let (event_kind, payload) = event?;
        if event_kind == "tool_result" {
            let Ok(result) = rmp_serde::from_slice::<crate::events::ToolResultPayload>(&payload)
            else {
                continue;
            };
            let Ok(value) = result.outcome else {
                continue;
            };
            if kind == "plugin_artifact"
                && (has_named_reference(&value, "attachment_id", artifact_id, 8)
                    || has_named_reference(&value, "artifact_id", artifact_id, 8))
            {
                return Ok(true);
            }
            if kind == "research_pdf"
                && research_job_id
                    .as_deref()
                    .is_some_and(|job_id| has_named_reference(&value, "job_id", job_id, 8))
            {
                return Ok(true);
            }
        } else if let Ok(card) = rmp_serde::from_slice::<crate::cards::CardClosedPayload>(&payload)
            && card.state == crate::cards::CardState::Completed
            && card.attachment_id.as_deref() == Some(artifact_id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn file_matches(path: &str, expected_sha256: &str, expected_bytes: i64) -> bool {
    if expected_bytes < 0 || expected_sha256.len() != 64 {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected_bytes as u64
    {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8_192];
    loop {
        let Ok(read) = file.read(&mut buffer) else {
            return false;
        };
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    hex::encode(digest.finalize()) == expected_sha256
}

/// Confirm a produced attachment still belongs to this run and its bytes
/// match the host's persisted digest. Inbound uploads are excluded.
pub(crate) fn run_artifact_exists(
    connection: &Connection,
    run_id: &str,
    reference: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    let Some(artifact_id) = reference
        .strip_prefix("attachment:")
        .filter(|id| !id.is_empty())
    else {
        return Ok(false);
    };
    let tool_result: Option<(String, String, i64)> = connection
        .query_row(
            "SELECT a.path,a.sha256,ta.byte_length FROM state_tool_result_artifacts ta \
             JOIN state_artifacts a ON a.id=ta.artifact_id \
             JOIN state_runs r ON r.run_id=ta.run_id AND r.conversation_id=ta.conversation_id \
             WHERE ta.artifact_id=?1 AND ta.run_id=?2 AND ta.expires_at>?3 \
               AND (a.expires_at IS NULL OR a.expires_at>?3) \
               AND a.sha256=ta.sha256 AND a.bytes=ta.byte_length",
            params![artifact_id, run_id, now],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((path, sha256, bytes)) = tool_result {
        return Ok(file_matches(&path, &sha256, bytes));
    }
    let child_result: Option<(String, String, i64)> = connection
        .query_row(
            "SELECT a.path,a.sha256,COALESCE(a.bytes,-1) FROM state_run_child_tasks task \
             JOIN state_runs child ON child.run_id=task.child_run_id AND child.status='completed' \
             JOIN state_artifacts a ON a.id=task.result_artifact_id \
             WHERE task.parent_run_id=?1 AND task.result_artifact_id=?2 \
               AND (a.expires_at IS NULL OR a.expires_at>?3)",
            params![run_id, artifact_id, now],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if child_result.is_some_and(|(path, sha256, bytes)| file_matches(&path, &sha256, bytes)) {
        return Ok(true);
    }
    artifact_has_run_producer(connection, run_id, artifact_id, now)
}

/// Confirm an agent's persisted output exists under that exact agent run.
pub(crate) fn agent_output_exists(
    connection: &Connection,
    run_id: &str,
    reference: &str,
) -> rusqlite::Result<bool> {
    if reference != format!("agent-run:{run_id}/output") {
        return Ok(false);
    }
    let output: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT status,output_text FROM state_agent_runs WHERE id=?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(output.is_some_and(|(status, text)| {
        status == "success" && text.is_some_and(|text| !text.trim().is_empty())
    }))
}

/// Check an immutable Controller audit record used as a human attestation.
pub(crate) fn attestation_exists(
    connection: &Connection,
    reference: &str,
    table_name: &str,
    row_id: &str,
    required_status: &str,
    submitted_refs: Option<&[String]>,
) -> rusqlite::Result<bool> {
    let Some(id) = reference
        .strip_prefix("attestation:")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0)
    else {
        return Ok(false);
    };
    let stored: Option<(String, Option<Vec<u8>>)> = connection
        .query_row(
            "SELECT actor,new_json FROM config_audit WHERE id=?1 AND table_name=?2 AND row_id=?3",
            params![id, table_name, row_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(stored.is_some_and(|(actor, payload)| {
        let Some(value) = payload
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        else {
            return false;
        };
        if actor.trim().is_empty()
            || value.get("status").and_then(serde_json::Value::as_str) != Some(required_status)
        {
            return false;
        }
        match required_status {
            "passed" => {
                let Some(expected) = submitted_refs else {
                    return false;
                };
                let submitted = expected
                    .iter()
                    .filter(|reference| !reference.starts_with("attestation:"))
                    .collect::<Vec<_>>();
                !submitted.is_empty()
                    && value
                        .get("submitted_evidence_refs")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|stored| {
                            stored.len() == submitted.len()
                                && stored
                                    .iter()
                                    .zip(submitted)
                                    .all(|(left, right)| left.as_str() == Some(right.as_str()))
                        })
            }
            "confirmed" => value
                .get("submitted_evidence_ref")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reference| !reference.trim().is_empty() && reference.len() <= 512),
            _ => false,
        }
    }))
}

/// Resolve a delivered transport outbox row inside this durable run's event
/// window. Unknown, pending, and foreign outbox rows cannot confirm delivery.
pub(crate) fn run_delivery_exists(
    connection: &Connection,
    run_id: &str,
    reference: &str,
) -> rusqlite::Result<bool> {
    let Some(outbox_id) = reference
        .strip_prefix("outbox:")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0)
    else {
        return Ok(false);
    };
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM state_runs run \
         JOIN state_outbox outbox ON outbox.conversation_id=run.conversation_id \
           AND outbox.enqueued_seq>=run.input_event_seq \
           AND outbox.enqueued_seq<COALESCE((SELECT MIN(next.input_event_seq) FROM state_runs next \
             WHERE next.conversation_id=run.conversation_id AND next.input_event_seq>run.input_event_seq),9223372036854775807) \
         JOIN state_outbox_delivery_events event ON event.outbox_id=outbox.id \
         WHERE run.run_id=?1 AND outbox.id=?2 AND outbox.status='delivered' \
           AND outbox.effect_kind LIKE 'transport.%' \
           AND event.transition IN ('delivered','operator_confirmed_delivered'))",
        params![run_id, outbox_id],
        |row| row.get(0),
    )
}
