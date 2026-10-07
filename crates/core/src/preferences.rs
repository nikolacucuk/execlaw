//! Governed operator preferences, separate from one-task instructions and inferred proposals.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferenceAuthority {
    Operator,
    Conversation,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preference {
    pub preference_id: String,
    pub owner_principal_id: String,
    pub scope: String,
    pub preference_key: String,
    pub value: serde_json::Value,
    pub origin: String,
    pub status: String,
    pub evidence_ref: String,
    pub expires_at: Option<i64>,
}
pub struct PreferenceStore<'db> {
    db: &'db Database,
}
impl<'db> PreferenceStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }
    /// Store an inference as a proposal. It cannot affect prompt loadouts until operator approval.
    pub fn propose_inferred(
        &self,
        owner: &str,
        scope: &str,
        key: &str,
        value: &serde_json::Value,
        evidence: &str,
        expires: Option<i64>,
        now: i64,
    ) -> Result<String, DbError> {
        self.write(
            owner, scope, key, value, "inferred", "proposed", evidence, expires, now,
        )
    }
    /// Persist an explicit preference only on an operator-authenticated path.
    pub fn set_explicit(
        &self,
        authority: PreferenceAuthority,
        owner: &str,
        scope: &str,
        key: &str,
        value: &serde_json::Value,
        evidence: &str,
        expires: Option<i64>,
        now: i64,
    ) -> Result<String, DbError> {
        if authority != PreferenceAuthority::Operator {
            return Err(DbError::Invariant(
                "only an operator can declare a persistent preference".into(),
            ));
        }
        if scope.trim().is_empty() || scope.starts_with("task:") {
            return Err(DbError::Invariant(
                "task instructions are not persistent preference scopes".into(),
            ));
        }
        self.write(
            owner, scope, key, value, "explicit", "approved", evidence, expires, now,
        )
    }
    fn write(
        &self,
        owner: &str,
        scope: &str,
        key: &str,
        value: &serde_json::Value,
        origin: &str,
        status: &str,
        evidence: &str,
        expires: Option<i64>,
        now: i64,
    ) -> Result<String, DbError> {
        if owner.trim().is_empty()
            || scope.trim().is_empty()
            || key.trim().is_empty()
            || evidence.trim().is_empty()
            || expires.is_some_and(|t| t <= now)
        {
            return Err(DbError::Invariant(
                "preference requires owner, scope, key, evidence, and a future expiry".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let value = serde_json::to_string(value).map_err(|e| DbError::Invariant(e.to_string()))?;
        self.db.with_conn(|c|{c.execute("INSERT INTO operator_preferences(preference_id,owner_principal_id,scope,preference_key,value_json,origin,status,evidence_ref,expires_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(owner_principal_id,scope,preference_key) DO UPDATE SET preference_id=excluded.preference_id,value_json=excluded.value_json,origin=excluded.origin,status=excluded.status,evidence_ref=excluded.evidence_ref,expires_at=excluded.expires_at,updated_at=excluded.updated_at",params![id,owner,scope,key,value,origin,status,evidence,expires,now])?;Ok(())})?;
        Ok(id)
    }
    /// Approve an inferred value. Only the operator-authorized caller may invoke this transition.
    pub fn approve_inference(
        &self,
        authority: PreferenceAuthority,
        id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if authority != PreferenceAuthority::Operator {
            return Err(DbError::Invariant(
                "only an operator can approve inferred preferences".into(),
            ));
        }
        self.db.with_conn(|c|{let changed=c.execute("UPDATE operator_preferences SET status='approved',updated_at=?2 WHERE preference_id=?1 AND origin='inferred' AND status='proposed'",params![id,now])?;if changed!=1{return Err(DbError::Invariant("preference inference is not pending".into()));}Ok(())})
    }
    /// Retract a preference so it is absent from every subsequent loadout.
    pub fn remove(
        &self,
        authority: PreferenceAuthority,
        id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if authority != PreferenceAuthority::Operator {
            return Err(DbError::Invariant(
                "only an operator can remove a persistent preference".into(),
            ));
        }
        self.db.with_conn(|c|{let n=c.execute("UPDATE operator_preferences SET status='retracted',updated_at=?2 WHERE preference_id=?1",params![id,now])?;if n!=1{return Err(DbError::Invariant("preference not found".into()));}Ok(())})
    }
    /// Load approved preferences for a requested scope and global scope; proposals stay hidden.
    pub fn loadout(&self, owner: &str, scope: &str, now: i64) -> Result<Vec<Preference>, DbError> {
        self.db.with_conn(|c|{let mut s=c.prepare("SELECT preference_id,owner_principal_id,scope,preference_key,value_json,origin,status,evidence_ref,expires_at FROM operator_preferences WHERE owner_principal_id=?1 AND scope IN ('global',?2) AND status='approved' AND (expires_at IS NULL OR expires_at>?3) ORDER BY CASE scope WHEN 'global' THEN 0 ELSE 1 END,preference_key")?;let rows=s.query_map(params![owner,scope,now],|r|{let value:String=r.get(4)?;Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,value,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,Option<i64>>(8)?))})?;let mut out=Vec::new();for row in rows{let r=row?;out.push(Preference{preference_id:r.0,owner_principal_id:r.1,scope:r.2,preference_key:r.3,value:serde_json::from_str(&r.4).map_err(|e|rusqlite::Error::FromSqlConversionFailure(4,rusqlite::types::Type::Text,Box::new(e)))?,origin:r.5,status:r.6,evidence_ref:r.7,expires_at:r.8});}Ok(out)})
    }

    /// List current and proposed preferences for the operator correction surface.
    pub fn list(&self, owner: &str, scope: Option<&str>) -> Result<Vec<Preference>, DbError> {
        let ids = self.db.with_conn(|connection| {
            let mut statement = if scope.is_some() {
                connection.prepare("SELECT preference_id FROM operator_preferences WHERE owner_principal_id=?1 AND scope IN ('global',?2) ORDER BY scope,preference_key")?
            } else {
                connection.prepare("SELECT preference_id FROM operator_preferences WHERE owner_principal_id=?1 ORDER BY scope,preference_key")?
            };
            if let Some(scope) = scope {
                let rows = statement.query_map(params![owner, scope], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            } else {
                let rows = statement.query_map([owner], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
        })?;
        ids.into_iter()
            .filter_map(|id| match self.get(&id) {
                Ok(Some(preference)) => Some(Ok(preference)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    /// Reject an inferred preference without removing its evidence record.
    pub fn reject_inference(
        &self,
        authority: PreferenceAuthority,
        id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if authority != PreferenceAuthority::Operator {
            return Err(DbError::Invariant(
                "only an operator can reject inferred preferences".into(),
            ));
        }
        self.db.with_conn(|connection| {
            let changed = connection.execute(
                "UPDATE operator_preferences SET status='rejected',updated_at=?2 WHERE preference_id=?1 AND origin='inferred' AND status='proposed'",
                params![id, now],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant("preference inference is not pending".into()));
            }
            Ok(())
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Preference>, DbError> {
        self.db.with_conn(|c|{let row=c.query_row("SELECT preference_id,owner_principal_id,scope,preference_key,value_json,origin,status,evidence_ref,expires_at FROM operator_preferences WHERE preference_id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,Option<i64>>(8)?))).optional()?;row.map(|r|Ok(Preference{preference_id:r.0,owner_principal_id:r.1,scope:r.2,preference_key:r.3,value:serde_json::from_str(&r.4).map_err(|e|DbError::Invariant(e.to_string()))?,origin:r.5,status:r.6,evidence_ref:r.7,expires_at:r.8})).transpose()})
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
    fn inferences_are_proposals_and_loadouts_obey_scope_expiry_correction_and_removal() {
        let d = fresh();
        let s = PreferenceStore::new(&d);
        let v = serde_json::json!("concise");
        let inferred = s
            .propose_inferred("operator", "global", "style", &v, "event:10", None, 1)
            .unwrap();
        assert!(s.loadout("operator", "global", 2).unwrap().is_empty());
        assert_eq!(s.list("operator", None).unwrap()[0].status, "proposed");
        s.approve_inference(PreferenceAuthority::Conversation, &inferred, 2)
            .unwrap_err();
        s.approve_inference(PreferenceAuthority::Operator, &inferred, 2)
            .unwrap();
        assert_eq!(s.loadout("operator", "global", 3).unwrap().len(), 1);
        let rejected = s
            .propose_inferred(
                "operator",
                "agent:research",
                "tone",
                &v,
                "event:11",
                None,
                3,
            )
            .unwrap();
        s.reject_inference(PreferenceAuthority::Conversation, &rejected, 4)
            .unwrap_err();
        s.reject_inference(PreferenceAuthority::Operator, &rejected, 4)
            .unwrap();
        assert_eq!(s.get(&rejected).unwrap().unwrap().status, "rejected");
        let task = s.set_explicit(
            PreferenceAuthority::Operator,
            "operator",
            "task:42",
            "style",
            &serde_json::json!("verbose"),
            "request:42",
            None,
            4,
        );
        assert!(task.is_err());
        assert!(
            s.set_explicit(
                PreferenceAuthority::Conversation,
                "operator",
                "global",
                "style",
                &v,
                "message:external",
                None,
                4
            )
            .is_err()
        );
        let id = s
            .set_explicit(
                PreferenceAuthority::Operator,
                "operator",
                "global",
                "style",
                &serde_json::json!("detailed"),
                "operator:5",
                Some(8),
                5,
            )
            .unwrap();
        assert_eq!(
            s.loadout("operator", "global", 6).unwrap()[0].value,
            serde_json::json!("detailed")
        );
        assert!(s.loadout("operator", "global", 9).unwrap().is_empty());
        s.remove(PreferenceAuthority::Operator, &id, 10).unwrap();
        assert!(s.loadout("operator", "global", 11).unwrap().is_empty());
    }
}
