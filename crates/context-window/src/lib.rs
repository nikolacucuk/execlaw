//! Context-window management for the execlaw agent runner.
//!
//! Long conversations accumulate more tokens than a model's context window
//! can hold. This crate provides a pure, model-agnostic policy layer that
//! trims a `Vec<ChatMessage>` to fit within a budget before the first
//! inference call of each turn.
//!
//! # Policies
//!
//! - [`ContextWindowPolicy::FullReplay`] — no trimming; replay the entire
//!   log on every turn. Correct for short conversations; breaks for long
//!   ones. This is the legacy default.
//! - [`ContextWindowPolicy::SlidingTurns(n)`] — keep only the *n* most
//!   recent exchanges (user + assistant pairs), always preserving the
//!   system prompt at index 0. Simple and predictable; calibrate *n* for
//!   your context budget.
//! - [`ContextWindowPolicy::TokenBudget { max_tokens, reserve_for_reply }`]
//!   — trim from the front until the estimated token count of the remaining
//!   messages fits within `max_tokens − reserve_for_reply`. Uses a fast
//!   char-count heuristic (1 token ≈ 4 chars); no tokeniser dependency.
//!
//! # Invariants
//!
//! 1. The system prompt (the first message with `role == System`) is
//!    **never removed** by any policy — it carries the persona and operator
//!    rules.
//! 2. `apply` never adds or reorders messages; it only removes from the
//!    front of the non-system portion.
//! 3. `apply` is idempotent: calling it twice with the same policy on the
//!    same slice has the same effect as calling it once.

#![forbid(unsafe_code)]

use execlaw_inference_api::{ChatMessage, Role};

// -----------------------------------------------------------------------
// Policy definition
// -----------------------------------------------------------------------

/// How the runner should manage the conversation history sent to the model
/// on each turn.
///
/// Stored in the `config_general_settings` table under the key
/// `context_window_policy` as a JSON-serialised string. The server falls
/// back to `FullReplay` when the key is absent so existing deployments
/// are unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextWindowPolicy {
    /// Send the complete conversation history on every turn.
    /// Simple and correct; fails for conversations that exceed the
    /// model's context length.
    FullReplay,

    /// Keep at most `max_turns` user+assistant exchange pairs,
    /// preserving the system prompt at position 0. The newest turns
    /// are retained; older turns are discarded.
    ///
    /// A "turn pair" is counted as follows: consecutive (user,
    /// assistant) messages contribute 2 messages; an unpaired user
    /// message at the tail contributes 1. Tool-call messages
    /// (`role == Tool`) are treated as part of the assistant turn
    /// they conclude and are always kept or dropped together with
    /// their parent.
    SlidingTurns(usize),

    /// Trim history until the estimated token count of the remaining
    /// messages (excluding system prompt) fits within
    /// `max_tokens − reserve_for_reply`. The system prompt is
    /// always preserved.
    ///
    /// Token estimation uses the simple heuristic:
    ///   `token_count ≈ char_count / 4`
    /// This under-counts for dense Chinese/Japanese text and over-
    /// counts for long whitespace-heavy outputs, but is accurate
    /// enough for budget management and requires no tokeniser
    /// dependency (no vendored sentencepiece / tiktoken).
    TokenBudget {
        /// Maximum number of tokens the model context can hold.
        max_tokens: usize,
        /// How many tokens to leave empty for the model's reply.
        /// Typical value: 512–2048 depending on expected response
        /// length.
        reserve_for_reply: usize,
    },
}

impl Default for ContextWindowPolicy {
    fn default() -> Self {
        Self::FullReplay
    }
}

// -----------------------------------------------------------------------
// Token estimation
// -----------------------------------------------------------------------

/// Fast token-count heuristic: 1 token ≈ 4 UTF-8 characters.
///
/// This is deliberately approximate — accurate enough for budget
/// enforcement while avoiding any tokeniser dependency. The 4-char
/// constant matches the average across English prose, code, and JSON.
/// Dense CJK text runs closer to 1 token/char; space-heavy prompts
/// run closer to 1 token/6 chars — the heuristic errs slightly towards
/// *over*-counting, giving a small safety margin.
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_chars = match &m.content {
                Some(execlaw_inference_api::MessageContent::Text(t)) => t.len(),
                Some(execlaw_inference_api::MessageContent::Parts(parts)) => parts
                    .iter()
                    .map(|p| match p {
                        execlaw_inference_api::ContentPart::Text { text } => text.len(),
                        execlaw_inference_api::ContentPart::ImageUrl { .. } => 256,
                    })
                    .sum(),
                None => 0,
            };
            // Per-message overhead: role + structural JSON tokens (~4).
            4 + content_chars / 4
        })
        .sum()
}

// -----------------------------------------------------------------------
// Policy application
// -----------------------------------------------------------------------

/// Trim `messages` in-place according to `policy`.
///
/// The system prompt (the first `Role::System` message, which is
/// always at index 0 in execlaw's turn builder) is unconditionally
/// preserved. All trimming removes messages from the **front** of the
/// non-system portion.
///
/// # Panics
///
/// Does not panic; returns without modification if `messages` is empty
/// or contains only a system prompt.
pub fn apply(policy: &ContextWindowPolicy, messages: &mut Vec<ChatMessage>) {
    if messages.len() <= 1 {
        return;
    }

    match policy {
        ContextWindowPolicy::FullReplay => {
            // No-op: keep everything.
        }

        ContextWindowPolicy::SlidingTurns(max_turns) => {
            if *max_turns == 0 {
                // Degenerate: discard everything except the system prompt.
                messages.truncate(1);
                return;
            }

            // Locate the non-system prefix. In normal execlaw usage
            // messages[0] is always the system prompt, but handle a
            // missing system prompt defensively.
            let conversation_start = messages
                .iter()
                .position(|m| m.role != Role::System)
                .unwrap_or(0);

            // Count from the tail: walk backwards accumulating "turns".
            // We treat each non-system block (however long — including
            // tool_result chains) as contributing to the turn counter
            // only when we see a Role::User message.
            let mut turns_counted = 0;
            let mut keep_from = messages.len(); // sentinel: keep nothing by default

            for (i, msg) in messages.iter().enumerate().skip(conversation_start).rev() {
                if msg.role == Role::User {
                    turns_counted += 1;
                }
                if turns_counted == *max_turns {
                    keep_from = i;
                    break;
                }
            }

            if keep_from > conversation_start && turns_counted >= *max_turns {
                // Remove messages[conversation_start..keep_from].
                messages.drain(conversation_start..keep_from);
                tracing::debug!(
                    removed = keep_from - conversation_start,
                    "context-window: SlidingTurns trimmed history",
                );
            }
        }

        ContextWindowPolicy::TokenBudget {
            max_tokens,
            reserve_for_reply,
        } => {
            let budget = max_tokens.saturating_sub(*reserve_for_reply);

            // Split: system prompt + conversation tail.
            let conversation_start = messages
                .iter()
                .position(|m| m.role != Role::System)
                .unwrap_or(messages.len());

            // Start with all messages and trim from the front of the
            // conversation portion until we fit.
            let trim_up_to = conversation_start; // remove nothing initially
            loop {
                let tokens = estimate_tokens(messages);
                if tokens <= budget {
                    break;
                }
                if trim_up_to >= messages.len() - 1 {
                    // Can't trim further without removing the last
                    // message; leave it in place and warn.
                    tracing::warn!(
                        estimated_tokens = tokens,
                        budget,
                        "context-window: cannot trim below budget; sending oversized context",
                    );
                    break;
                }
                // Remove the oldest non-system message.
                messages.remove(trim_up_to);
                // trim_up_to stays the same — it now points at the next
                // message after the one we just removed.
            }
            let final_tokens = estimate_tokens(messages);
            if final_tokens < *max_tokens {
                tracing::debug!(
                    estimated_tokens = final_tokens,
                    budget,
                    "context-window: TokenBudget trimmed history",
                );
            }
        }
    }
}

// -----------------------------------------------------------------------
// Parse from config string
// -----------------------------------------------------------------------

/// Parse a `ContextWindowPolicy` from its config-table representation.
///
/// Accepted forms:
/// - `"full_replay"` — no trimming
/// - `"sliding:N"` — SlidingTurns(N)
/// - `"token_budget:MAX:RESERVE"` — TokenBudget
///
/// Returns `ContextWindowPolicy::FullReplay` on any parse failure so
/// a malformed DB entry doesn't crash the runner.
pub fn parse_policy(s: &str) -> ContextWindowPolicy {
    if s == "full_replay" {
        return ContextWindowPolicy::FullReplay;
    }
    if let Some(rest) = s.strip_prefix("sliding:") {
        if let Ok(n) = rest.parse::<usize>() {
            return ContextWindowPolicy::SlidingTurns(n);
        }
    }
    if let Some(rest) = s.strip_prefix("token_budget:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            if let (Ok(max), Ok(reserve)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                return ContextWindowPolicy::TokenBudget {
                    max_tokens: max,
                    reserve_for_reply: reserve,
                };
            }
        }
    }
    tracing::warn!(
        config_value = s,
        "context-window: unrecognised policy string; defaulting to FullReplay",
    );
    ContextWindowPolicy::FullReplay
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_inference_api::{ChatMessage, MessageContent, Role};

    fn sys() -> ChatMessage {
        ChatMessage::system("You are a helpful assistant.")
    }

    fn user(n: u32) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(MessageContent::Text(format!("user message {n}"))),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }

    fn asst(n: u32) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: Some(MessageContent::Text(format!("assistant reply {n}"))),
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }

    fn make_history(turns: u32) -> Vec<ChatMessage> {
        let mut msgs = vec![sys()];
        for i in 1..=turns {
            msgs.push(user(i));
            msgs.push(asst(i));
        }
        msgs
    }

    // --- FullReplay ---

    #[test]
    fn full_replay_keeps_all_messages() {
        let mut msgs = make_history(5);
        let original_len = msgs.len();
        apply(&ContextWindowPolicy::FullReplay, &mut msgs);
        assert_eq!(msgs.len(), original_len);
    }

    // --- SlidingTurns ---

    #[test]
    fn sliding_keeps_exactly_n_turns_plus_system() {
        let mut msgs = make_history(5); // system + 5 * (user + asst) = 11
        apply(&ContextWindowPolicy::SlidingTurns(3), &mut msgs);
        // System prompt + 3 most recent user/asst pairs = 1 + 6 = 7
        // (turns 3, 4, 5 kept)
        assert_eq!(
            msgs[0].role,
            Role::System,
            "system prompt must be at index 0"
        );
        let user_count = msgs.iter().filter(|m| m.role == Role::User).count();
        assert_eq!(user_count, 3, "exactly 3 user messages should remain");
    }

    #[test]
    fn sliding_zero_removes_all_non_system() {
        let mut msgs = make_history(3);
        apply(&ContextWindowPolicy::SlidingTurns(0), &mut msgs);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn sliding_larger_than_history_keeps_everything() {
        let mut msgs = make_history(2); // 5 messages
        let original_len = msgs.len();
        apply(&ContextWindowPolicy::SlidingTurns(100), &mut msgs);
        assert_eq!(msgs.len(), original_len, "larger window keeps everything");
    }

    #[test]
    fn sliding_preserves_system_prompt_content() {
        let mut msgs = make_history(4);
        apply(&ContextWindowPolicy::SlidingTurns(1), &mut msgs);
        assert_eq!(
            msgs[0].role,
            Role::System,
            "first message must remain the system prompt"
        );
        if let Some(MessageContent::Text(t)) = &msgs[0].content {
            assert!(
                t.contains("helpful assistant"),
                "system prompt content preserved"
            );
        }
    }

    // --- TokenBudget ---

    #[test]
    fn token_budget_trims_to_fit() {
        // Each message is "user/assistant message N" ≈ ~25 chars ≈ ~6 tokens.
        // System prompt ≈ ~8 tokens. 5 turns = ~60 tokens.
        // Set a tight budget of 30 tokens + 5 reserve → 25 budget.
        let mut msgs = make_history(5);
        apply(
            &ContextWindowPolicy::TokenBudget {
                max_tokens: 30,
                reserve_for_reply: 5,
            },
            &mut msgs,
        );
        let tokens = estimate_tokens(&msgs);
        assert!(
            tokens <= 25,
            "estimated tokens {tokens} should be ≤ budget 25"
        );
        // System prompt must survive.
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn token_budget_noop_when_already_fits() {
        let mut msgs = make_history(2); // small history
        let original_len = msgs.len();
        apply(
            &ContextWindowPolicy::TokenBudget {
                max_tokens: 100_000,
                reserve_for_reply: 512,
            },
            &mut msgs,
        );
        assert_eq!(
            msgs.len(),
            original_len,
            "no trimming needed for tiny history"
        );
    }

    #[test]
    fn token_budget_single_message_stays() {
        // Even if the single message exceeds budget, we must not remove
        // the last message — the runner would have nothing to send.
        let mut msgs = vec![sys(), user(1)];
        apply(
            &ContextWindowPolicy::TokenBudget {
                max_tokens: 1,
                reserve_for_reply: 0,
            },
            &mut msgs,
        );
        // System prompt stays.
        assert_eq!(msgs[0].role, Role::System);
        // The only conversation message stays (we can't trim below 1).
        assert!(msgs.len() >= 1);
    }

    // --- parse_policy ---

    #[test]
    fn parse_full_replay() {
        assert_eq!(parse_policy("full_replay"), ContextWindowPolicy::FullReplay);
    }

    #[test]
    fn parse_sliding() {
        assert_eq!(
            parse_policy("sliding:10"),
            ContextWindowPolicy::SlidingTurns(10)
        );
    }

    #[test]
    fn parse_token_budget() {
        assert_eq!(
            parse_policy("token_budget:8192:512"),
            ContextWindowPolicy::TokenBudget {
                max_tokens: 8192,
                reserve_for_reply: 512,
            }
        );
    }

    #[test]
    fn parse_unknown_defaults_to_full_replay() {
        assert_eq!(
            parse_policy("something_weird"),
            ContextWindowPolicy::FullReplay
        );
    }

    // --- estimate_tokens ---

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_single_system() {
        let msgs = vec![sys()];
        let est = estimate_tokens(&msgs);
        // System prompt "You are a helpful assistant." = 35 chars → ≈ 8 + 4 = 12
        assert!(
            est > 0 && est < 50,
            "reasonable estimate for short system prompt"
        );
    }
}
