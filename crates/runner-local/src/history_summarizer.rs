//! History summarizer — compresses a segment of conversation history into
//! a single system-style `ChatMessage` using the operator's Small inference
//! backend.
//!
//! # Purpose
//!
//! When `ContextWindowPolicy::TokenBudget` prunes old messages, the agent
//! loses context from those turns. The summarizer runs the pruned segment
//! through the Small backend (a fast, cheap model separate from the main
//! Standard backend) to produce a concise summary.
//!
//! The summary is injected as a `ChatMessage::system` at position 1 (after
//! the operator system prompt) so the model can reference it even though the
//! raw turns are gone.
//!
//! # Usage
//!
//! ```ignore
//! let summary_msg = summarize_segment(&dropped_messages, &inference_client, &model_id).await?;
//! messages.insert(1, summary_msg); // inject after system prompt
//! ```
//!
//! # Design notes
//!
//! - The summarizer is intentionally stateless: it receives a slice of
//!   `ChatMessage`s and returns one `ChatMessage`. The caller decides
//!   when to call it and where to insert the result.
//! - Using a `Small` backend keeps cost/latency low — the summary prompt
//!   only needs to produce a few sentences.
//! - If inference fails, the caller should log the error and proceed
//!   without a summary rather than aborting the turn.

use execlaw_inference_api::{ChatMessage, ChatRequest, InferenceClient, InferenceError, ModelId};

/// Maximum tokens to allow the summary model to produce.
/// Small models are fast; a 256-token summary is usually enough.
const SUMMARY_MAX_TOKENS: u32 = 256;

/// Temperature used for summarization — low for factual compression.
const SUMMARY_TEMPERATURE: f32 = 0.2;

/// Summarise `turns` (a contiguous slice of messages that will be
/// dropped from the active context) into a single `ChatMessage` that
/// can be inserted at position 1 in the active message list.
///
/// `client` + `model_id` should correspond to the operator's
/// `BackendPurpose::Small` backend. The caller is responsible for
/// resolving the right client and model before calling this function.
///
/// Returns `Err(InferenceError)` if the inference call fails.  The
/// caller should treat this as non-fatal: skip the summary and proceed
/// with the trimmed history rather than aborting the turn.
pub async fn summarize_segment(
    turns: &[ChatMessage],
    client: &InferenceClient,
    model_id: &ModelId,
) -> Result<ChatMessage, InferenceError> {
    if turns.is_empty() {
        return Ok(ChatMessage::system(
            "[Summary: no prior conversation history.]",
        ));
    }

    // Build a compact text representation of the turns for the prompt.
    // We intentionally avoid full JSON serialisation here — we only
    // care about the textual content for the summary, not the metadata.
    let mut transcript = String::new();
    for msg in turns {
        let role_str = match msg.role {
            execlaw_inference_api::Role::User => "User",
            execlaw_inference_api::Role::Assistant => "Assistant",
            execlaw_inference_api::Role::System => "System",
            execlaw_inference_api::Role::Tool => "Tool",
        };
        let text = msg
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default();
        if !text.is_empty() {
            transcript.push_str(role_str);
            transcript.push_str(": ");
            transcript.push_str(&text);
            transcript.push('\n');
        }
    }

    let prompt = format!(
        "Summarise the following conversation segment in 3–6 concise bullet points, \
         preserving any facts, decisions, or commitments made. \
         Do not include preamble — output only the bullets.\n\n\
         --- BEGIN SEGMENT ---\n{transcript}--- END SEGMENT ---"
    );

    let req = ChatRequest {
        model: model_id.clone(),
        messages: vec![
            ChatMessage::system(
                "You are a precise summarisation assistant. \
                 Condense conversation history into bullet-point summaries.",
            ),
            ChatMessage::user(prompt),
        ],
        temperature: Some(SUMMARY_TEMPERATURE),
        max_tokens: Some(SUMMARY_MAX_TOKENS),
        tools: None,
        tool_choice: None,
        stream: false,
        chat_template_kwargs: None,
        guided_decoding_backend: None,
    };

    let resp = client.chat_completions(&req).await?;

    let summary_text = resp
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .map(|c| c.as_text())
        .unwrap_or_else(|| "[Summary: (model returned no content)]".to_owned());

    let prefix = format!("Conversation summary ({} messages):\n", turns.len());
    Ok(ChatMessage::system(format!("{prefix}{summary_text}")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_inference_api::{ChatMessage, InferenceClient, MessageContent, ModelId, Role};

    /// Minimal mock server that always returns a fixed summary response.
    async fn run_mock_summary_server(body: &'static str) -> String {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn summarize_empty_segment_returns_placeholder() {
        // No server needed — empty input returns immediately.
        let client = InferenceClient::new("http://127.0.0.1:1".to_owned());
        let model = ModelId("test-model".to_owned());
        let result = summarize_segment(&[], &client, &model).await.unwrap();
        match &result.content {
            Some(c) => assert!(c.as_text().contains("no prior conversation")),
            None => panic!("expected Some content"),
        }
    }

    #[tokio::test]
    async fn summarize_calls_inference_and_returns_system_message() {
        let summary_body = r#"{
            "id": "s1",
            "model": "small-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "- User asked about the weather\n- Assistant said it is sunny"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 50, "completion_tokens": 20, "total_tokens": 70}
        }"#;

        let base_url = run_mock_summary_server(summary_body).await;
        let client = InferenceClient::new(base_url);
        let model = ModelId("small-model".to_owned());

        let turns = vec![
            ChatMessage::user("What is the weather?"),
            ChatMessage {
                role: Role::Assistant,
                content: Some(MessageContent::Text("It is sunny today.".to_owned())),
                tool_call_id: None,
                name: None,
                tool_calls: vec![],
            },
        ];

        let result = summarize_segment(&turns, &client, &model).await.unwrap();

        assert_eq!(result.role, Role::System, "summary must be a system message");
        let text = result.content.as_ref().unwrap().as_text();
        assert!(
            text.contains("Conversation summary"),
            "prefix present: {text}"
        );
        assert!(
            text.contains("weather") || text.contains("sunny"),
            "summary content present: {text}"
        );
    }

    #[tokio::test]
    async fn summarize_prefixes_message_count() {
        let summary_body = r#"{
            "id": "s2",
            "model": "small-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "- A\n- B"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;

        let base_url = run_mock_summary_server(summary_body).await;
        let client = InferenceClient::new(base_url);
        let model = ModelId("small-model".to_owned());

        let turns: Vec<ChatMessage> = (0..4).map(|i| ChatMessage::user(format!("msg {i}"))).collect();
        let result = summarize_segment(&turns, &client, &model).await.unwrap();
        let text = result.content.as_ref().unwrap().as_text();
        assert!(text.contains("4 messages"), "message count in prefix: {text}");
    }
}
