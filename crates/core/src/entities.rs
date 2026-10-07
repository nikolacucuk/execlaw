//! Evidence-backed entity aliases and reversible, explicitly governed merge proposals.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityCandidate {
    pub entity_id: String,
    pub kind: String,
    pub canonical_label: String,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityMergeProposal {
    pub proposal_id: String,
    pub left_entity_id: String,
    pub right_entity_id: String,
    pub status: String,
    pub evidence_ref: String,
    pub proposed_by: Option<String>,
    pub created_at: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<i64>,
}

pub struct EntityStore<'db> {
    db: &'db Database,
}
impl<'db> EntityStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }
    pub fn create(&self, kind: &str, canonical_label: &str, now: i64) -> Result<String, DbError> {
        if !["person", "project", "place", "resource"].contains(&kind)
            || canonical_label.trim().is_empty()
        {
            return Err(DbError::Invariant(
                "entity requires a supported kind and non-empty label".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|c| { c.execute("INSERT INTO knowledge_entities(entity_id,entity_kind,canonical_label,created_at) VALUES(?1,?2,?3,?4)",params![id,kind,canonical_label.trim(),now])?; Ok(()) })?;
        Ok(id)
    }
    pub fn add_alias(
        &self,
        entity_id: &str,
        label: &str,
        valid_from: i64,
        valid_to: Option<i64>,
        evidence_ref: &str,
        authoritative: bool,
    ) -> Result<String, DbError> {
        if label.trim().is_empty()
            || evidence_ref.trim().is_empty()
            || valid_to.is_some_and(|end| end <= valid_from)
        {
            return Err(DbError::Invariant(
                "alias needs a label, evidence, and valid time interval".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|c| { c.execute("INSERT INTO knowledge_entity_aliases(alias_id,entity_id,label,valid_from,valid_to,evidence_ref,authoritative) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,entity_id,label.trim(),valid_from,valid_to,evidence_ref,authoritative])?; Ok(()) })?;
        Ok(id)
    }
    /// Return all entities matching a label at the requested time; ambiguity is preserved.
    pub fn candidates(&self, label: &str, at: i64) -> Result<Vec<EntityCandidate>, DbError> {
        self.db.with_conn(|c| { let mut s=c.prepare("SELECT DISTINCT e.entity_id,e.entity_kind,e.canonical_label,a.evidence_ref FROM knowledge_entity_aliases a JOIN knowledge_entities e USING(entity_id) WHERE a.label=?1 AND a.valid_from<=?2 AND (a.valid_to IS NULL OR a.valid_to>?2) AND e.retired_at IS NULL AND NOT EXISTS(SELECT 1 FROM knowledge_entity_redirects r WHERE r.source_entity_id=e.entity_id) ORDER BY e.entity_id")?; let rows=s.query_map(params![label,at],|r|Ok(EntityCandidate{entity_id:r.get(0)?,kind:r.get(1)?,canonical_label:r.get(2)?,evidence_ref:r.get(3)?}))?; rows.collect::<Result<Vec<_>,_>>().map_err(Into::into) })
    }
    pub fn propose_merge(
        &self,
        left: &str,
        right: &str,
        evidence_ref: &str,
        actor_id: &str,
        now: i64,
    ) -> Result<String, DbError> {
        if left == right || evidence_ref.trim().is_empty() || actor_id.trim().is_empty() {
            return Err(DbError::Invariant(
                "merge requires distinct entities, evidence, and an operator actor".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|c| {c.execute("INSERT INTO knowledge_entity_merge_proposals(proposal_id,left_entity_id,right_entity_id,status,evidence_ref,created_at,proposed_by) VALUES(?1,?2,?3,'proposed',?4,?5,?6)",params![id,left,right,evidence_ref,now,actor_id])?;Ok(())})?;
        Ok(id)
    }
    /// List merge proposals for Controller review, including their recorded decision actor.
    pub fn proposals(&self, status: Option<&str>) -> Result<Vec<EntityMergeProposal>, DbError> {
        if status.is_some_and(|status| {
            !["proposed", "accepted", "rejected", "reversed"].contains(&status)
        }) {
            return Err(DbError::Invariant("unknown merge proposal status".into()));
        }
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT proposal_id,left_entity_id,right_entity_id,status,evidence_ref,proposed_by,created_at,decided_by,decided_at FROM knowledge_entity_merge_proposals WHERE (?1 IS NULL OR status=?1) ORDER BY created_at DESC,proposal_id",
            )?;
            let rows = statement.query_map([status], |row| Ok(EntityMergeProposal {
                proposal_id: row.get(0)?, left_entity_id: row.get(1)?, right_entity_id: row.get(2)?,
                status: row.get(3)?, evidence_ref: row.get(4)?, proposed_by: row.get(5)?,
                created_at: row.get(6)?, decided_by: row.get(7)?, decided_at: row.get(8)?,
            }))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        })
    }
    /// Accept a proposal; identity authority is not stored or copied by this semantic relation.
    pub fn accept_merge(
        &self,
        proposal_id: &str,
        actor_id: &str,
        reason: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if actor_id.trim().is_empty() || reason.trim().is_empty() {
            return Err(DbError::Invariant(
                "merge decision requires an operator actor and reason".into(),
            ));
        }
        self.db.transaction(|c| { let (left,right)=c.query_row("SELECT left_entity_id,right_entity_id FROM knowledge_entity_merge_proposals WHERE proposal_id=?1 AND status='proposed'",[proposal_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?.ok_or_else(||DbError::Invariant("merge proposal is not pending".into()))?;
            let involved:i64=c.query_row("SELECT COUNT(*) FROM knowledge_entity_redirects WHERE source_entity_id IN (?1,?2) OR target_entity_id IN (?1,?2)",params![left,right],|r|r.get(0))?;
            if involved>0{return Err(DbError::Invariant("merge rejected because an entity already participates in a merge; reverse that merge first".into()));}
            c.execute("UPDATE knowledge_entity_merge_proposals SET status='accepted',decided_at=?2,decided_by=?3 WHERE proposal_id=?1",params![proposal_id,now,actor_id])?;
            c.execute("INSERT INTO knowledge_entity_redirects(source_entity_id,target_entity_id,proposal_id) VALUES(?1,?2,?3)",params![right,left,proposal_id])?;
            c.execute("INSERT INTO knowledge_entity_merge_decisions(decision_id,proposal_id,actor_id,decision,reason,created_at) VALUES(?1,?2,?3,'accepted',?4,?5)",params![Uuid::new_v4().to_string(),proposal_id,actor_id,reason,now])?; Ok(()) })
    }
    /// Reject a pending merge proposal while preserving its evidence and decision actor.
    pub fn reject_merge(
        &self,
        proposal_id: &str,
        actor_id: &str,
        reason: &str,
        now: i64,
    ) -> Result<(), DbError> {
        self.decide_pending_merge(proposal_id, actor_id, "rejected", reason, now)
    }
    fn decide_pending_merge(
        &self,
        proposal_id: &str,
        actor_id: &str,
        decision: &str,
        reason: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if actor_id.trim().is_empty() || reason.trim().is_empty() {
            return Err(DbError::Invariant(
                "merge decision requires an operator actor and reason".into(),
            ));
        }
        self.db.transaction(|c| {
            let changed=c.execute("UPDATE knowledge_entity_merge_proposals SET status=?2,decided_at=?3,decided_by=?4 WHERE proposal_id=?1 AND status='proposed'",params![proposal_id,decision,now,actor_id])?;
            if changed!=1{return Err(DbError::Invariant("merge proposal is not pending".into()));}
            c.execute("INSERT INTO knowledge_entity_merge_decisions(decision_id,proposal_id,actor_id,decision,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![Uuid::new_v4().to_string(),proposal_id,actor_id,decision,reason,now])?; Ok(())
        })
    }
    /// Reverse an accepted merge without rewriting aliases or source evidence.
    pub fn reverse_merge(
        &self,
        proposal_id: &str,
        actor_id: &str,
        reason: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if actor_id.trim().is_empty() || reason.trim().is_empty() {
            return Err(DbError::Invariant(
                "merge reversal requires an operator actor and reason".into(),
            ));
        }
        self.db.transaction(|c| { let changed=c.execute("DELETE FROM knowledge_entity_redirects WHERE proposal_id=?1",[proposal_id])?; if changed!=1{return Err(DbError::Invariant("accepted merge redirect is missing".into()));}
            let changed=c.execute("UPDATE knowledge_entity_merge_proposals SET status='reversed',decided_at=?2,decided_by=?3 WHERE proposal_id=?1 AND status='accepted'",params![proposal_id,now,actor_id])?; if changed!=1{return Err(DbError::Invariant("merge proposal is not accepted".into()));}
            c.execute("INSERT INTO knowledge_entity_merge_decisions(decision_id,proposal_id,actor_id,decision,reason,created_at) VALUES(?1,?2,?3,'reversed',?4,?5)",params![Uuid::new_v4().to_string(),proposal_id,actor_id,reason,now])?; Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::DbConfig, migrations::MigrationRunner};
    fn fresh() -> Database {
        let d = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&d).apply_all().unwrap();
        d
    }
    #[test]
    fn same_name_stays_ambiguous_and_merge_is_reversible() {
        let d = fresh();
        let s = EntityStore::new(&d);
        let a = s.create("person", "Jordan A", 1).unwrap();
        let b = s.create("person", "Jordan B", 1).unwrap();
        s.add_alias(&a, "Jordan", 1, None, "event:1", false)
            .unwrap();
        s.add_alias(&b, "Jordan", 1, None, "event:2", false)
            .unwrap();
        assert_eq!(s.candidates("Jordan", 2).unwrap().len(), 2);
        let p = s
            .propose_merge(&a, &b, "event:9", "controller-1", 3)
            .unwrap();
        assert_eq!(
            s.proposals(Some("proposed")).unwrap()[0]
                .proposed_by
                .as_deref(),
            Some("controller-1")
        );
        assert_eq!(s.candidates("Jordan", 2).unwrap().len(), 2);
        s.accept_merge(&p, "controller-1", "same person verified", 4)
            .unwrap();
        let accepted = s.proposals(Some("accepted")).unwrap();
        assert_eq!(accepted[0].decided_by.as_deref(), Some("controller-1"));
        assert_eq!(s.candidates("Jordan", 2).unwrap().len(), 1);
        let reverse_cycle = s
            .propose_merge(&b, &a, "event:10", "controller-1", 4)
            .unwrap();
        assert!(
            s.accept_merge(&reverse_cycle, "controller-1", "cycle", 5)
                .is_err()
        );
        s.reverse_merge(&p, "controller-1", "operator correction", 5)
            .unwrap();
        assert_eq!(s.candidates("Jordan", 2).unwrap().len(), 2);
        let decision_count: i64 = d
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM knowledge_entity_merge_decisions WHERE proposal_id=?1",
                    [&p],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(decision_count, 2);
        let rejected = s
            .propose_merge(&a, &b, "event:11", "controller-1", 6)
            .unwrap();
        s.reject_merge(&rejected, "controller-1", "insufficient evidence", 7)
            .unwrap();
        assert!(
            s.accept_merge(&rejected, "controller-1", "late", 8)
                .is_err()
        );
        assert!(s.add_alias(&a, "old", 10, Some(9), "e", true).is_err());
    }
}
