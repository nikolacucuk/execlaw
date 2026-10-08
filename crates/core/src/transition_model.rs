//! Small bounded executable contract for run, approval, lease, and outbox ownership.
//!
//! This deliberately models only the durable protocol. SQLite and external
//! transports have separate implementation tests; the model explores the
//! adversarial ordering of duplicate requests, workers, expiry, restart, and
//! cancellation that those tests need to preserve.

use std::collections::{BTreeSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    ClaimA,
    ClaimB,
    ExpireLease,
    Restart,
    Approve,
    Deny,
    Enqueue,
    Deliver,
    Cancel,
    Finish,
    CompleteA,
    CompleteB,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Work {
    Pending,
    ClaimedA,
    ClaimedB,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Approval {
    Pending,
    Approved,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Effect {
    Absent,
    Pending,
    Delivered,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    work: Work,
    lease_live: bool,
    approval: Approval,
    effect: Effect,
    terminal: bool,
    cancelled: bool,
}

impl State {
    fn initial() -> Self {
        Self {
            work: Work::Pending,
            lease_live: false,
            approval: Approval::Pending,
            effect: Effect::Absent,
            terminal: false,
            cancelled: false,
        }
    }

    fn step(self, action: Action) -> Self {
        if action == Action::CompleteA {
            return self.complete_by(Work::ClaimedA).unwrap_or(self);
        }
        if action == Action::CompleteB {
            return self.complete_by(Work::ClaimedB).unwrap_or(self);
        }
        let mut next = self;
        match action {
            Action::ClaimA if !next.terminal && !next.cancelled && !next.lease_live => {
                next.work = Work::ClaimedA;
                next.lease_live = true;
            }
            Action::ClaimB if !next.terminal && !next.cancelled && !next.lease_live => {
                next.work = Work::ClaimedB;
                next.lease_live = true;
            }
            Action::ExpireLease if next.lease_live => next.lease_live = false,
            // A restart never transfers ownership or marks work complete.
            Action::Restart => {}
            Action::Approve if next.approval == Approval::Pending => {
                next.approval = Approval::Approved
            }
            Action::Deny if next.approval == Approval::Pending => next.approval = Approval::Denied,
            Action::Enqueue
                if !next.terminal
                    && !next.cancelled
                    && next.approval == Approval::Approved
                    && next.work == Work::Completed
                    && next.effect == Effect::Absent =>
            {
                next.effect = Effect::Pending
            }
            Action::Deliver if next.effect == Effect::Pending => next.effect = Effect::Delivered,
            Action::Cancel if !next.terminal => {
                next.cancelled = true;
                next.lease_live = false;
                if !matches!(next.work, Work::Completed) {
                    next.work = Work::Cancelled;
                }
            }
            Action::Finish
                if !next.cancelled
                    && next.work == Work::Completed
                    && next.approval == Approval::Approved
                    && matches!(next.effect, Effect::Absent | Effect::Delivered) =>
            {
                next.terminal = true
            }
            _ => {}
        }
        next
    }

    fn complete_by(&self, worker: Work) -> Option<Self> {
        if self.terminal || self.cancelled || !self.lease_live || self.work != worker {
            return None;
        }
        let mut next = self.clone();
        next.work = Work::Completed;
        next.lease_live = false;
        Some(next)
    }

    fn is_safe(&self) -> bool {
        let unresolved = self.work != Work::Completed
            || self.approval != Approval::Approved
            || self.effect == Effect::Pending;
        !self.terminal || !unresolved
    }
}

/// Summary from bounded breadth-first exploration of the critical transition contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionModelReport {
    pub explored_states: usize,
    pub explored_transitions: usize,
    pub max_depth: usize,
}

/// Explore all distinct valid traces through the requested depth and check invariants.
pub fn check_critical_transitions(max_depth: usize) -> Result<TransitionModelReport, String> {
    let actions = [
        Action::ClaimA,
        Action::ClaimB,
        Action::ExpireLease,
        Action::Restart,
        Action::Approve,
        Action::Deny,
        Action::Enqueue,
        Action::Deliver,
        Action::Cancel,
        Action::Finish,
        Action::CompleteA,
        Action::CompleteB,
    ];
    let mut queue = VecDeque::from([(State::initial(), Vec::<Action>::new())]);
    let mut seen = BTreeSet::from([State::initial()]);
    let mut transitions = 0usize;
    while let Some((state, trace)) = queue.pop_front() {
        let depth = trace.len();
        if !state.is_safe() {
            return Err(format!("unsafe trace {trace:?}: {state:?}"));
        }
        if depth >= max_depth {
            continue;
        }
        for action in actions {
            let next = state.clone().step(action);
            transitions += 1;
            if !next.is_safe() {
                let mut counterexample = trace.clone();
                counterexample.push(action);
                return Err(format!("counterexample {counterexample:?}: {next:?}"));
            }
            if seen.insert(next.clone()) {
                let mut next_trace = trace.clone();
                next_trace.push(action);
                queue.push_back((next, next_trace));
            }
        }
    }
    Ok(TransitionModelReport {
        explored_states: seen.len(),
        explored_transitions: transitions,
        max_depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_interleavings_keep_single_lease_owner_and_require_all_work_before_finish() {
        let report = check_critical_transitions(10).expect("model invariants hold");
        assert!(report.explored_states > 20);
        assert!(report.explored_transitions > report.explored_states);
        assert_eq!(report.max_depth, 10);
    }

    #[test]
    fn expired_or_cancelled_worker_cannot_complete_after_ownership_changes() {
        let active = State::initial().step(Action::ClaimA);
        let expired = active
            .clone()
            .step(Action::ExpireLease)
            .step(Action::ClaimB);
        assert_eq!(
            active.complete_by(Work::ClaimedA).unwrap().work,
            Work::Completed
        );
        assert!(expired.complete_by(Work::ClaimedA).is_none());
        assert!(expired.complete_by(Work::ClaimedB).is_some());
        let cancelled = active.clone().step(Action::Cancel);
        assert!(cancelled.complete_by(Work::ClaimedA).is_none());
    }

    #[test]
    fn completion_cannot_skip_approval_or_pending_required_effect() {
        let completed = State::initial()
            .step(Action::ClaimA)
            .complete_by(Work::ClaimedA)
            .unwrap();
        assert!(!completed.clone().step(Action::Finish).terminal);
        let approved = completed
            .clone()
            .step(Action::Approve)
            .step(Action::Enqueue);
        assert!(!approved.clone().step(Action::Finish).terminal);
        let terminal = approved.step(Action::Deliver).step(Action::Finish);
        assert!(terminal.terminal);
        assert_eq!(terminal.clone().step(Action::Enqueue), terminal);
    }
}
