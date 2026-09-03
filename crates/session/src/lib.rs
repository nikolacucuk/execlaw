//! execlaw-session
//!
//! A `Session` binds a conversation to the runner and to whichever pipeline
//! components are required by its modality (text → `runner-local`; voice →
//! `voice-pipeline` wrapping `runner-local`). Phase 0 ships only a type
//! outline; Phase 1 adds the state machine.

#![forbid(unsafe_code)]

use execlaw_core::ConversationId;
use execlaw_core::conversation::Modality;

// ────────────────────────────────────────────────────────────────────────────
// Phase 1 — Session state machine
// ────────────────────────────────────────────────────────────────────────────

/// All states a conversation session can occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// No turn is running; ready to accept a new one.
    Idle,
    /// A turn is in progress.
    Active,
    /// A tool call requires operator approval before execution continues.
    AwaitingApproval,
    /// The final turn has completed; the session is draining.
    Completing,
    /// Permanently closed — no further events accepted.
    Closed,
}

/// Events that drive the session state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionEvent {
    TurnStarted,
    ApprovalRequired,
    ApprovalResolved,
    TurnCompleted,
    ConversationClosed,
}

/// Returned when a `SessionEvent` is illegal in the current `SessionState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub state: SessionState,
    pub event: SessionEvent,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "illegal event {:?} in state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for SessionError {}

/// A handle to an in-flight conversation's plumbing.
#[derive(Debug, Clone)]
pub struct Session {
    pub conversation_id: ConversationId,
    pub modality: Modality,
    state: SessionState,
}

impl Session {
    pub fn new(conversation_id: ConversationId, modality: Modality) -> Self {
        Self {
            conversation_id,
            modality,
            state: SessionState::Idle,
        }
    }

    /// Current FSM state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Drive the FSM forward.
    ///
    /// Returns `Err(SessionError)` if `event` is not valid in the current
    /// state. Valid transitions:
    ///
    /// | From              | Event               | To                |
    /// |-------------------|---------------------|-------------------|
    /// | Idle              | TurnStarted         | Active            |
    /// | Active            | ApprovalRequired    | AwaitingApproval  |
    /// | Active            | TurnCompleted       | Idle              |
    /// | Active            | ConversationClosed  | Closed            |
    /// | AwaitingApproval  | ApprovalResolved    | Active            |
    /// | AwaitingApproval  | ConversationClosed  | Closed            |
    /// | Idle              | ConversationClosed  | Closing → Closed  |
    /// | Completing        | ConversationClosed  | Closed            |
    pub fn transition(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        use SessionEvent::*;
        use SessionState::*;

        let next = match (self.state, event) {
            (Idle, TurnStarted) => Active,
            (Idle, ConversationClosed) => Closed,
            (Active, ApprovalRequired) => AwaitingApproval,
            (Active, TurnCompleted) => Idle,
            (Active, ConversationClosed) => Closed,
            (AwaitingApproval, ApprovalResolved) => Active,
            (AwaitingApproval, ConversationClosed) => Closed,
            (Completing, ConversationClosed) => Closed,
            _ => {
                return Err(SessionError {
                    state: self.state,
                    event,
                });
            }
        };

        self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(ConversationId::from("c"), Modality::Text)
    }

    #[test]
    fn session_binds_modality() {
        let s = Session::new(ConversationId::from("c"), Modality::Voice);
        assert_eq!(s.modality, Modality::Voice);
    }

    #[test]
    fn initial_state_is_idle() {
        assert_eq!(session().state(), SessionState::Idle);
    }

    #[test]
    fn idle_turn_started_becomes_active() {
        let mut s = session();
        s.transition(SessionEvent::TurnStarted).unwrap();
        assert_eq!(s.state(), SessionState::Active);
    }

    #[test]
    fn active_turn_completed_returns_to_idle() {
        let mut s = session();
        s.transition(SessionEvent::TurnStarted).unwrap();
        s.transition(SessionEvent::TurnCompleted).unwrap();
        assert_eq!(s.state(), SessionState::Idle);
    }

    #[test]
    fn active_approval_required_becomes_awaiting() {
        let mut s = session();
        s.transition(SessionEvent::TurnStarted).unwrap();
        s.transition(SessionEvent::ApprovalRequired).unwrap();
        assert_eq!(s.state(), SessionState::AwaitingApproval);
    }

    #[test]
    fn awaiting_approval_resolved_returns_to_active() {
        let mut s = session();
        s.transition(SessionEvent::TurnStarted).unwrap();
        s.transition(SessionEvent::ApprovalRequired).unwrap();
        s.transition(SessionEvent::ApprovalResolved).unwrap();
        assert_eq!(s.state(), SessionState::Active);
    }

    #[test]
    fn conversation_closed_from_active_becomes_closed() {
        let mut s = session();
        s.transition(SessionEvent::TurnStarted).unwrap();
        s.transition(SessionEvent::ConversationClosed).unwrap();
        assert_eq!(s.state(), SessionState::Closed);
    }

    #[test]
    fn conversation_closed_from_idle_becomes_closed() {
        let mut s = session();
        s.transition(SessionEvent::ConversationClosed).unwrap();
        assert_eq!(s.state(), SessionState::Closed);
    }

    #[test]
    fn closed_rejects_all_events() {
        let mut s = session();
        s.transition(SessionEvent::ConversationClosed).unwrap();
        for ev in [
            SessionEvent::TurnStarted,
            SessionEvent::ApprovalRequired,
            SessionEvent::ApprovalResolved,
            SessionEvent::TurnCompleted,
            SessionEvent::ConversationClosed,
        ] {
            assert!(s.transition(ev).is_err(), "closed must reject {ev:?}");
        }
    }

    #[test]
    fn invalid_transition_returns_err() {
        let mut s = session();
        // Cannot fire TurnCompleted while Idle.
        let err = s.transition(SessionEvent::TurnCompleted).unwrap_err();
        assert_eq!(err.state, SessionState::Idle);
        assert_eq!(err.event, SessionEvent::TurnCompleted);
    }

    #[test]
    fn error_display_is_readable() {
        let e = SessionError {
            state: SessionState::Idle,
            event: SessionEvent::ApprovalResolved,
        };
        let msg = e.to_string();
        assert!(msg.contains("ApprovalResolved"));
        assert!(msg.contains("Idle"));
    }
}
