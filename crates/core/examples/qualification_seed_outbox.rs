//! Seed one disposable relay effect after a committed test conversation.

use execlaw_core::ids::{ConversationId, EventSeq, IdempotencyKey};
use execlaw_core::outbox::{OutboxRow, OutboxStatus, OutboxStore};
use execlaw_core::{Database, DbConfig};
use serde::Serialize;

#[derive(Serialize)]
struct TransportSendEffect<'a> {
    channel: &'a str,
    recipient: &'a str,
    text: &'a str,
    model_seq: Option<i64>,
    archive_message_id: Option<&'a str>,
    owner: Option<()>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let db_path = args.next().ok_or("database path required")?;
    let conversation_id = ConversationId::from(args.next().ok_or("conversation id required")?);
    let event_seq = args
        .next()
        .ok_or("committed event sequence required")?
        .parse::<i64>()?;
    let db = Database::open(&DbConfig {
        path: db_path.into(),
        key: None,
    })?;
    let payload = rmp_serde::to_vec(&TransportSendEffect {
        channel: "discord",
        recipient: "qa-channel",
        text: "qualification-effect",
        model_seq: Some(event_seq),
        archive_message_id: None,
        owner: None,
    })?;
    let row = OutboxRow {
        id: None,
        idempotency_key: IdempotencyKey::mint_scoped(
            &conversation_id,
            b"relay-qualification-v1",
            0,
        ),
        conversation_id,
        effect_kind: "transport.send".into(),
        payload,
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: None,
        last_error: None,
        enqueued_seq: EventSeq(event_seq),
    };
    let (id, created) = OutboxStore::new(&db).enqueue_idempotent(&row)?;
    println!("OUTBOX_ID={id} CREATED={created}");
    Ok(())
}
