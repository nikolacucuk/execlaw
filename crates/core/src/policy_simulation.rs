//! Decision metadata and non-executing previews for candidate tool policies.

use crate::db::{Database, DbError};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Final host decision made for one attempted tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionOutcome {
    Allowed,
    Denied,
    ApprovalGated,
}

impl PolicyDecisionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::ApprovalGated => "approval_gated",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "allowed" => Some(Self::Allowed),
            "denied" => Some(Self::Denied),
            "approval_gated" => Some(Self::ApprovalGated),
            _ => None,
        }
    }
}

/// Authorized decision metadata recorded without tool arguments or outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ToolPolicyDecision {
    pub decision_id: String,
    pub run_id: String,
    pub conversation_id: String,
    pub input_event_seq: i64,
    pub tool_name: String,
    pub caller_trust: String,
    pub trust_floor: Option<String>,
    pub required_capabilities: Vec<String>,
    pub globally_enabled: bool,
    pub allowed_classes: Vec<String>,
    pub profile_id: Option<String>,
    pub profile_revision: Option<i64>,
    pub outcome: PolicyDecisionOutcome,
    pub reason_code: String,
    pub sensitive: bool,
    pub external_effect: bool,
    pub approval_required: bool,
    pub policy_revision: i64,
    pub decided_at: i64,
}

/// Host-owned decision input safe to retain for a future policy simulation.
#[derive(Debug, Clone)]
pub struct NewToolPolicyDecision {
    pub run_id: String,
    pub conversation_id: String,
    pub input_event_seq: i64,
    pub tool_name: String,
    pub caller_trust: String,
    pub trust_floor: Option<String>,
    pub required_capabilities: Vec<String>,
    pub globally_enabled: bool,
    pub allowed_classes: Vec<String>,
    pub profile_id: Option<String>,
    pub profile_revision: Option<i64>,
    pub outcome: PolicyDecisionOutcome,
    pub reason_code: String,
    pub sensitive: bool,
    pub external_effect: bool,
    pub approval_required: bool,
    pub decided_at: i64,
}

/// One proposed policy rule for a single registered tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CandidateToolPolicy {
    pub tool_name: String,
    pub enabled: bool,
    pub allowed_classes: Vec<String>,
    pub trust_floor: Option<String>,
}

/// Effect of a proposed policy on one saved decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PolicySimulationChange {
    pub decision_id: String,
    pub tool_name: String,
    pub caller_trust: String,
    pub change: String,
    pub explanation: String,
}

/// Bounded result of a policy simulation. It never dispatches a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PolicySimulationReport {
    pub evaluated_decisions: usize,
    pub newly_allowed: Vec<PolicySimulationChange>,
    pub newly_denied: Vec<PolicySimulationChange>,
    pub newly_approval_gated: Vec<PolicySimulationChange>,
    pub omitted_changes: usize,
}

/// Durable decision-history errors.
#[derive(Debug, Error)]
pub enum PolicySimulationError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("invalid policy decision: {0}")]
    Invalid(String),
}

/// SQLite access to sanitized tool policy decisions.
pub struct ToolPolicyDecisionStore<'db> {
    db: &'db Database,
}

impl<'db> ToolPolicyDecisionStore<'db> {
    /// Create a decision-history store.
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Record only policy inputs and outcomes; tool arguments and results are excluded.
    pub fn record(
        &self,
        decision: &NewToolPolicyDecision,
    ) -> Result<String, PolicySimulationError> {
        if decision.run_id.trim().is_empty()
            || decision.conversation_id.trim().is_empty()
            || decision.input_event_seq <= 0
            || decision.tool_name.trim().is_empty()
            || decision.reason_code.len() > 128
        {
            return Err(PolicySimulationError::Invalid(
                "required policy decision metadata is invalid".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let required = serde_json::to_string(&decision.required_capabilities)
            .map_err(|error| PolicySimulationError::Invalid(error.to_string()))?;
        let allowed = serde_json::to_string(&decision.allowed_classes)
            .map_err(|error| PolicySimulationError::Invalid(error.to_string()))?;
        self.db.with_conn(|connection| {
            let policy_revision: i64 = connection.query_row(
                "SELECT COALESCE(MAX(revision_id), 0) FROM config_tool_access_policy_revisions \
                 WHERE tool_name = ?1",
                [&decision.tool_name],
                |row| row.get(0),
            )?;
            connection.execute(
                "INSERT INTO state_tool_policy_decisions \
                 (decision_id, run_id, conversation_id, input_event_seq, tool_name, caller_trust, trust_floor, \
                  required_capabilities_json, globally_enabled, allowed_classes_json, profile_id, \
                  profile_revision, outcome, reason_code, sensitive, external_effect, approval_required, \
                  policy_revision, decided_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![
                    id,
                    decision.run_id,
                    decision.conversation_id,
                    decision.input_event_seq,
                    decision.tool_name,
                    decision.caller_trust,
                    decision.trust_floor,
                    required,
                    decision.globally_enabled as i64,
                    allowed,
                    decision.profile_id,
                    decision.profile_revision,
                    decision.outcome.as_str(),
                    decision.reason_code,
                    decision.sensitive as i64,
                    decision.external_effect as i64,
                    decision.approval_required as i64,
                    policy_revision,
                    decision.decided_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// Load recent decisions for one tool, newest first and without content fields.
    pub fn list_for_tool(
        &self,
        tool_name: &str,
        limit: usize,
    ) -> Result<Vec<ToolPolicyDecision>, PolicySimulationError> {
        let limit = limit.clamp(1, 2_000) as i64;
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT decision_id, run_id, conversation_id, input_event_seq, tool_name, caller_trust, trust_floor, \
                 required_capabilities_json, globally_enabled, allowed_classes_json, profile_id, \
                 profile_revision, outcome, reason_code, sensitive, external_effect, approval_required, \
                 policy_revision, decided_at FROM state_tool_policy_decisions \
                 WHERE tool_name = ?1 ORDER BY decided_at DESC, decision_id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![tool_name, limit], row_to_decision)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        }).map_err(PolicySimulationError::from)
    }
}

/// Replay saved decision metadata through a candidate policy without executing tools.
pub fn simulate_policy_change(
    decisions: &[ToolPolicyDecision],
    candidate: &CandidateToolPolicy,
) -> PolicySimulationReport {
    let mut report = PolicySimulationReport {
        evaluated_decisions: 0,
        newly_allowed: Vec::new(),
        newly_denied: Vec::new(),
        newly_approval_gated: Vec::new(),
        omitted_changes: 0,
    };
    for decision in decisions
        .iter()
        .filter(|item| item.tool_name == candidate.tool_name)
    {
        report.evaluated_decisions += 1;
        let profile_still_allows = decision.reason_code != "safety_profile_denied";
        let trust_allows = candidate
            .allowed_classes
            .iter()
            .any(|class| class == &decision.caller_trust)
            && candidate
                .trust_floor
                .as_deref()
                .is_none_or(|floor| trust_rank(&decision.caller_trust) >= trust_rank(floor));
        let candidate_allowed = candidate.enabled && profile_still_allows && trust_allows;
        let prior_allowed = matches!(
            decision.outcome,
            PolicyDecisionOutcome::Allowed | PolicyDecisionOutcome::ApprovalGated
        );
        let prior_gated =
            decision.approval_required || decision.outcome == PolicyDecisionOutcome::ApprovalGated;
        let candidate_gated = candidate_allowed
            && (decision.approval_required
                || ((decision.sensitive || decision.external_effect)
                    && trust_rank(&decision.caller_trust) < trust_rank("KnownTrusted")));
        let change = if candidate_gated && !prior_gated {
            Some((
                "newly_approval_gated",
                "candidate trust floor allows this sensitive or external action only after Controller approval",
            ))
        } else if candidate_allowed && !prior_allowed {
            Some((
                "newly_allowed",
                "candidate trust and enable rules admit this previously denied action",
            ))
        } else if !candidate_allowed && prior_allowed {
            Some((
                "newly_denied",
                "candidate trust or enable rules block this previously allowed action",
            ))
        } else {
            None
        };
        let Some((kind, explanation)) = change else {
            continue;
        };
        let item = PolicySimulationChange {
            decision_id: decision.decision_id.clone(),
            tool_name: decision.tool_name.clone(),
            caller_trust: decision.caller_trust.clone(),
            change: kind.to_owned(),
            explanation: explanation.to_owned(),
        };
        let bucket = match kind {
            "newly_allowed" => &mut report.newly_allowed,
            "newly_denied" => &mut report.newly_denied,
            _ => &mut report.newly_approval_gated,
        };
        if bucket.len() < 500 {
            bucket.push(item);
        } else {
            report.omitted_changes += 1;
        }
    }
    report
}

fn row_to_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolPolicyDecision> {
    let trust: String = row.get(5)?;
    if !matches!(
        trust.as_str(),
        "Controller" | "Delegated" | "KnownTrusted" | "KnownLimited" | "UnknownPending" | "Blocked"
    ) {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown trust class",
            )),
        ));
    }
    let outcome: String = row.get(12)?;
    let outcome = PolicyDecisionOutcome::parse(&outcome).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            12,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown policy outcome",
            )),
        )
    })?;
    let required: String = row.get(7)?;
    let required_capabilities = serde_json::from_str(&required).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let allowed: String = row.get(9)?;
    let allowed_classes = serde_json::from_str(&allowed).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ToolPolicyDecision {
        decision_id: row.get(0)?,
        run_id: row.get(1)?,
        conversation_id: row.get(2)?,
        input_event_seq: row.get(3)?,
        tool_name: row.get(4)?,
        caller_trust: trust,
        trust_floor: row.get(6)?,
        required_capabilities,
        globally_enabled: row.get::<_, i64>(8)? != 0,
        allowed_classes,
        profile_id: row.get(10)?,
        profile_revision: row.get(11)?,
        outcome,
        reason_code: row.get(13)?,
        sensitive: row.get::<_, i64>(14)? != 0,
        external_effect: row.get::<_, i64>(15)? != 0,
        approval_required: row.get::<_, i64>(16)? != 0,
        policy_revision: row.get(17)?,
        decided_at: row.get(18)?,
    })
}

fn trust_rank(value: &str) -> u8 {
    match value {
        "Controller" => 5,
        "Delegated" => 4,
        "KnownTrusted" => 3,
        "KnownLimited" => 2,
        "UnknownPending" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::MigrationRunner;

    fn saved_decision(outcome: PolicyDecisionOutcome, trust: &str) -> ToolPolicyDecision {
        ToolPolicyDecision {
            decision_id: format!("d-{trust}"),
            run_id: "run-1".into(),
            conversation_id: "conversation-1".into(),
            input_event_seq: 1,
            tool_name: "calendar.create".into(),
            caller_trust: trust.into(),
            trust_floor: Some("Controller".into()),
            required_capabilities: vec!["integration.approved".into()],
            globally_enabled: true,
            allowed_classes: vec!["Controller".into()],
            profile_id: None,
            profile_revision: None,
            outcome,
            reason_code: "trust_class_denied".into(),
            sensitive: true,
            external_effect: true,
            approval_required: false,
            policy_revision: 1,
            decided_at: 10,
        }
    }

    #[test]
    fn simulation_reports_allow_deny_and_approval_changes_without_dispatch() {
        let history = vec![
            saved_decision(PolicyDecisionOutcome::Denied, "KnownLimited"),
            saved_decision(PolicyDecisionOutcome::Denied, "KnownTrusted"),
            saved_decision(PolicyDecisionOutcome::Allowed, "Controller"),
        ];
        let report = simulate_policy_change(
            &history,
            &CandidateToolPolicy {
                tool_name: "calendar.create".into(),
                enabled: true,
                allowed_classes: vec!["KnownLimited".into(), "KnownTrusted".into()],
                trust_floor: None,
            },
        );
        assert_eq!(report.evaluated_decisions, 3);
        assert_eq!(report.newly_allowed.len(), 1);
        assert_eq!(report.newly_denied.len(), 1);
        assert_eq!(report.newly_approval_gated.len(), 1);
        assert_eq!(report.newly_approval_gated[0].caller_trust, "KnownLimited");
    }

    #[test]
    fn simulation_cannot_use_candidate_allowlist_to_override_profile_denial() {
        let mut decision = saved_decision(PolicyDecisionOutcome::Denied, "Controller");
        decision.reason_code = "safety_profile_denied".into();
        let report = simulate_policy_change(
            &[decision],
            &CandidateToolPolicy {
                tool_name: "calendar.create".into(),
                enabled: true,
                allowed_classes: vec!["Controller".into()],
                trust_floor: None,
            },
        );
        assert!(report.newly_allowed.is_empty());
    }

    #[test]
    fn decision_history_is_append_only_and_stores_no_tool_arguments() {
        let db = Database::open(&crate::db::DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let conversation = crate::ids::ConversationId::from("conversation");
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_conversations \
                 (conversation_id, kind, phase, trust_class, modality) \
                 VALUES (?1, 'ControllerDM', 'idle', 'Controller', 'Text')",
                [conversation.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        let event = crate::events::EventRecord::new(
            conversation.clone(),
            crate::ids::EventSeq(1),
            crate::events::EventKind::UserMsg,
            &serde_json::json!({"text":"fixture"}),
            Some("controller".into()),
        )
        .unwrap();
        crate::events::EventLog::new(&db).append(&event).unwrap();
        let store = ToolPolicyDecisionStore::new(&db);
        let id = store
            .record(&NewToolPolicyDecision {
                run_id: "turn:conversation:1".into(),
                conversation_id: "conversation".into(),
                input_event_seq: 1,
                tool_name: "calendar.create".into(),
                caller_trust: "KnownLimited".into(),
                trust_floor: Some("KnownLimited".into()),
                required_capabilities: vec!["integration.approved".into()],
                globally_enabled: true,
                allowed_classes: vec!["KnownLimited".into()],
                profile_id: Some("approved_integration".into()),
                profile_revision: Some(1),
                outcome: PolicyDecisionOutcome::Allowed,
                reason_code: "allowed".into(),
                sensitive: true,
                external_effect: true,
                approval_required: false,
                decided_at: 10,
            })
            .unwrap();
        let denied_update = db.with_conn(|connection| {
            Ok(connection.execute(
                "UPDATE state_tool_policy_decisions SET outcome='denied' WHERE decision_id=?1",
                [&id],
            )?)
        });
        assert!(denied_update.is_err());
        let decisions = store.list_for_tool("calendar.create", 10).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].outcome, PolicyDecisionOutcome::Allowed);
        assert_eq!(
            decisions[0].required_capabilities,
            vec!["integration.approved"]
        );
    }
}
