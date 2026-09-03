//! Durable definitions, mailbox, runs, and checkpoints for always-on agents.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub role_prompt: String,
    pub model: Option<String>,
    pub backend_purpose: String,
    pub tools: Vec<String>,
    pub trust_policy: serde_json::Value,
    pub interval_secs: u32,
    pub token_budget: u32,
    pub max_runtime_secs: u32,
    pub concurrency_limit: u32,
    pub enabled: bool,
    pub paused: bool,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_run_status: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunRow {
    pub id: String,
    pub agent_id: String,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub tokens_used: Option<u32>,
    pub checkpoint: serde_json::Value,
    pub output_text: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessageRow {
    pub id: String,
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    pub direction: String,
    pub content: String,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    pub result_run_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("invalid agent: {0}")]
    Invalid(String),
    #[error("encoding: {0}")]
    Encoding(String),
}

#[derive(Debug, Clone)]
pub struct AgentUpsert {
    pub id: Option<String>,
    pub name: String,
    pub role_prompt: String,
    pub model: Option<String>,
    pub backend_purpose: String,
    pub tools: Vec<String>,
    pub trust_policy: serde_json::Value,
    pub interval_secs: u32,
    pub token_budget: u32,
    pub max_runtime_secs: u32,
    pub concurrency_limit: u32,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct AgentStore {
    db: Database,
}

impl AgentStore {
    pub fn new(db: &Database) -> Self {
        Self { db: db.clone() }
    }

    pub fn list(&self) -> Result<Vec<AgentRow>, AgentError> {
        self.db.with_conn(|c| {
            let mut stmt = c.prepare("SELECT id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,last_run_at,last_run_status,last_error,created_at,updated_at FROM config_agents ORDER BY name")?;
            let rows = stmt.query_map([], map_agent)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }).map_err(Into::into)
    }

    pub fn get(&self, id: &str) -> Result<Option<AgentRow>, AgentError> {
        self.db.with_conn(|c| Ok(c.query_row("SELECT id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,last_run_at,last_run_status,last_error,created_at,updated_at FROM config_agents WHERE id=?1", [id], map_agent).optional()?)).map_err(Into::into)
    }

    pub fn upsert(&self, input: &AgentUpsert, now: i64) -> Result<AgentRow, AgentError> {
        if input.name.trim().is_empty() || input.role_prompt.trim().is_empty() {
            return Err(AgentError::Invalid(
                "name and role_prompt are required".into(),
            ));
        }
        if input.interval_secs == 0
            || input.token_budget == 0
            || input.max_runtime_secs == 0
            || input.concurrency_limit == 0
        {
            return Err(AgentError::Invalid(
                "budgets and concurrency must be greater than zero".into(),
            ));
        }
        let id = input
            .id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let tools =
            serde_json::to_string(&input.tools).map_err(|e| AgentError::Encoding(e.to_string()))?;
        let trust = serde_json::to_string(&input.trust_policy)
            .map_err(|e| AgentError::Encoding(e.to_string()))?;
        self.db.with_conn(|c| {
            c.execute("INSERT INTO config_agents (id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13,?13,?13) ON CONFLICT(id) DO UPDATE SET name=excluded.name,role_prompt=excluded.role_prompt,model=excluded.model,backend_purpose=excluded.backend_purpose,tools_json=excluded.tools_json,trust_policy_json=excluded.trust_policy_json,interval_secs=excluded.interval_secs,token_budget=excluded.token_budget,max_runtime_secs=excluded.max_runtime_secs,concurrency_limit=excluded.concurrency_limit,enabled=excluded.enabled,next_run_at=COALESCE(config_agents.next_run_at, excluded.next_run_at),updated_at=excluded.updated_at", params![id,input.name,input.role_prompt,input.model,input.backend_purpose,tools,trust,input.interval_secs,input.token_budget,input.max_runtime_secs,input.concurrency_limit,input.enabled as i64,now])?;
            Ok(())
        })?;
        self.get(&id)?.ok_or(AgentError::NotFound(id))
    }

    pub fn set_state(
        &self,
        id: &str,
        enabled: Option<bool>,
        paused: Option<bool>,
        next_run_at: Option<i64>,
    ) -> Result<(), AgentError> {
        self.db.with_conn(|c| { c.execute("UPDATE config_agents SET enabled=COALESCE(?1,enabled), paused=COALESCE(?2,paused), next_run_at=COALESCE(?3,next_run_at), updated_at=strftime('%s','now') WHERE id=?4", params![enabled.map(|v| v as i64), paused.map(|v| v as i64), next_run_at, id])?; Ok(()) }).map_err(Into::into)
    }

    pub fn delete(&self, id: &str) -> Result<bool, AgentError> {
        self.db
            .with_conn(|c| Ok(c.execute("DELETE FROM config_agents WHERE id=?1", [id])? > 0))
            .map_err(Into::into)
    }

    pub fn claim_due(&self, id: &str, now: i64) -> Result<Option<AgentRow>, AgentError> {
        self.db.with_conn(|c| { let n = c.execute("UPDATE config_agents SET next_run_at=?1, last_run_status='running', last_error=NULL, updated_at=?1 WHERE id=?2 AND enabled=1 AND paused=0 AND (next_run_at IS NULL OR next_run_at<=?1)", params![now.saturating_add(1),id])?; if n == 0 { return Ok(None); } Ok(Some(c.query_row("SELECT id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,last_run_at,last_run_status,last_error,created_at,updated_at FROM config_agents WHERE id=?1", [id], map_agent)?)) }).map_err(Into::into)
    }

    pub fn finish(
        &self,
        agent_id: &str,
        run_id: &str,
        status: &str,
        now: i64,
        next_run_at: i64,
        tokens: Option<u32>,
        output: Option<&str>,
        error: Option<&str>,
        checkpoint: &serde_json::Value,
    ) -> Result<(), AgentError> {
        let checkpoint =
            serde_json::to_string(checkpoint).map_err(|e| AgentError::Encoding(e.to_string()))?;
        self.db.with_conn(|c| { c.execute("UPDATE state_agent_runs SET status=?1,finished_at=?2,tokens_used=?3,output_text=?4,error=?5,checkpoint_json=?6 WHERE id=?7", params![status,now,tokens,output,error,checkpoint,run_id])?; c.execute("UPDATE config_agents SET last_run_at=?1,last_run_status=?2,last_error=?3,next_run_at=?4,updated_at=?1 WHERE id=?5", params![now,status,error,next_run_at,agent_id])?; Ok(()) }).map_err(Into::into)
    }

    pub fn insert_run(
        &self,
        agent_id: &str,
        now: i64,
        checkpoint: &serde_json::Value,
    ) -> Result<String, AgentError> {
        let id = Uuid::new_v4().to_string();
        let cp =
            serde_json::to_string(checkpoint).map_err(|e| AgentError::Encoding(e.to_string()))?;
        self.db.with_conn(|c| { c.execute("INSERT INTO state_agent_runs (id,agent_id,status,started_at,checkpoint_json) VALUES (?1,?2,'running',?3,?4)",params![id,agent_id,now,cp])?; Ok(()) })?;
        Ok(id)
    }

    pub fn pending_messages(
        &self,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentMessageRow>, AgentError> {
        self.db.with_conn(|c| { let mut s=c.prepare("SELECT id,agent_id,parent_agent_id,direction,content,created_at,delivered_at,result_run_id FROM state_agent_messages WHERE agent_id=?1 AND delivered_at IS NULL ORDER BY created_at LIMIT ?2")?; let rows=s.query_map(params![agent_id,limit],map_message)?; rows.collect::<Result<Vec<_>,_>>().map_err(Into::into) }).map_err(Into::into)
    }

    pub fn enqueue(
        &self,
        agent_id: &str,
        parent_agent_id: Option<&str>,
        content: &str,
        now: i64,
    ) -> Result<String, AgentError> {
        if content.trim().is_empty() {
            return Err(AgentError::Invalid("message is empty".into()));
        }
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|c|{c.execute("INSERT INTO state_agent_messages (id,agent_id,parent_agent_id,direction,content,created_at) VALUES (?1,?2,?3,'inbound',?4,?5)",params![id,agent_id,parent_agent_id,content,now])?;Ok(())})?;
        Ok(id)
    }

    pub fn deliver(&self, ids: &[String], run_id: &str, now: i64) -> Result<(), AgentError> {
        self.db.with_conn(|c|{for id in ids{c.execute("UPDATE state_agent_messages SET delivered_at=?1,result_run_id=?2 WHERE id=?3",params![now,run_id,id])?;}Ok(())}).map_err(Into::into)
    }

    pub fn runs(&self, agent_id: &str, limit: u32) -> Result<Vec<AgentRunRow>, AgentError> {
        self.db.with_conn(|c|{let mut s=c.prepare("SELECT id,agent_id,status,started_at,finished_at,tokens_used,checkpoint_json,output_text,error FROM state_agent_runs WHERE agent_id=?1 ORDER BY started_at DESC LIMIT ?2")?;let rows=s.query_map(params![agent_id,limit],map_run)?;rows.collect::<Result<Vec<_>,_>>().map_err(Into::into)}).map_err(Into::into)
    }
}

fn map_agent(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRow> {
    let tools: String = r.get(5)?;
    let trust: String = r.get(6)?;
    Ok(AgentRow {
        id: r.get(0)?,
        name: r.get(1)?,
        role_prompt: r.get(2)?,
        model: r.get(3)?,
        backend_purpose: r.get(4)?,
        tools: serde_json::from_str(&tools).unwrap_or_default(),
        trust_policy: serde_json::from_str(&trust).unwrap_or_else(|_| serde_json::json!({})),
        interval_secs: r.get(7)?,
        token_budget: r.get(8)?,
        max_runtime_secs: r.get(9)?,
        concurrency_limit: r.get(10)?,
        enabled: r.get::<_, i64>(11)? != 0,
        paused: r.get::<_, i64>(12)? != 0,
        next_run_at: r.get(13)?,
        last_run_at: r.get(14)?,
        last_run_status: r.get(15)?,
        last_error: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
    })
}
fn map_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRunRow> {
    let cp: String = r.get(6)?;
    Ok(AgentRunRow {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        status: r.get(2)?,
        started_at: r.get(3)?,
        finished_at: r.get(4)?,
        tokens_used: r.get(5)?,
        checkpoint: serde_json::from_str(&cp).unwrap_or_else(|_| serde_json::json!({})),
        output_text: r.get(7)?,
        error: r.get(8)?,
    })
}
fn map_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessageRow> {
    Ok(AgentMessageRow {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        parent_agent_id: r.get(2)?,
        direction: r.get(3)?,
        content: r.get(4)?,
        created_at: r.get(5)?,
        delivered_at: r.get(6)?,
        result_run_id: r.get(7)?,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};
    #[test]
    fn agent_round_trip() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let s = AgentStore::new(&db);
        let a = s
            .upsert(
                &AgentUpsert {
                    id: None,
                    name: "watcher".into(),
                    role_prompt: "Watch mailbox".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: vec!["notify".into()],
                    trust_policy: serde_json::json!({"trust":"Controller"}),
                    interval_secs: 60,
                    token_budget: 100,
                    max_runtime_secs: 30,
                    concurrency_limit: 1,
                    enabled: true,
                },
                1,
            )
            .unwrap();
        let m = s.enqueue(&a.id, None, "hello", 2).unwrap();
        assert_eq!(s.pending_messages(&a.id, 10).unwrap()[0].id, m);
        let run = s
            .insert_run(&a.id, 3, &serde_json::json!({"cursor":m}))
            .unwrap();
        s.deliver(&[m], &run, 4).unwrap();
        s.finish(
            &a.id,
            &run,
            "success",
            5,
            65,
            Some(3),
            Some("done"),
            None,
            &serde_json::json!({"cursor":"done"}),
        )
        .unwrap();
        assert_eq!(s.runs(&a.id, 10).unwrap()[0].status, "success");
    }
}
