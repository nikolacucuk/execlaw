//! SQLite persistence for operator-approved local endpoint ranges and names.
//!
//! DNS and HTTP stay in `execlaw-local-endpoint-policy`; core owns only
//! durable configuration and the last readable classification result.

use crate::{Database, DbError};
use rusqlite::params;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointApprovalKind {
    Cidr,
    DnsName,
}

impl EndpointApprovalKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cidr => "cidr",
            Self::DnsName => "dns_name",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalEndpointApprovals {
    pub cidrs: Vec<String>,
    pub dns_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointResolutionRecord {
    pub endpoint_key: String,
    pub url: String,
    pub classification: String,
    pub resolved_addresses: Vec<String>,
    pub last_error: Option<String>,
    pub resolved_at: i64,
}

pub struct LocalEndpointPolicyStore<'db> {
    db: &'db Database,
}

impl<'db> LocalEndpointPolicyStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn approvals(&self) -> Result<LocalEndpointApprovals, DbError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT kind, value FROM config_local_endpoint_approvals ORDER BY kind, value",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut approvals = LocalEndpointApprovals::default();
            for row in rows {
                let (kind, value) = row?;
                match kind.as_str() {
                    "cidr" => approvals.cidrs.push(value),
                    "dns_name" => approvals.dns_names.push(value),
                    _ => {}
                }
            }
            Ok(approvals)
        })
    }

    pub fn approve(
        &self,
        kind: EndpointApprovalKind,
        value: &str,
        now: i64,
    ) -> Result<(), DbError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(DbError::Config("endpoint approval may not be empty".into()));
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO config_local_endpoint_approvals(kind, value, created_at) VALUES (?1, ?2, ?3)",
                params![kind.as_str(), value, now],
            )?;
            Ok(())
        })
    }

    pub fn revoke(&self, kind: EndpointApprovalKind, value: &str) -> Result<bool, DbError> {
        self.db.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM config_local_endpoint_approvals WHERE kind = ?1 AND value = ?2",
                params![kind.as_str(), value],
            )? > 0)
        })
    }

    pub fn record_resolution(&self, record: &EndpointResolutionRecord) -> Result<(), DbError> {
        let addresses = serde_json::to_string(&record.resolved_addresses)
            .map_err(|error| DbError::Serde(error.to_string()))?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_local_endpoint_resolutions(endpoint_key, url, classification, resolved_addresses, last_error, resolved_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(endpoint_key) DO UPDATE SET url=excluded.url, classification=excluded.classification, \
                 resolved_addresses=excluded.resolved_addresses, last_error=excluded.last_error, resolved_at=excluded.resolved_at",
                params![record.endpoint_key, record.url, record.classification, addresses, record.last_error, record.resolved_at],
            )?;
            Ok(())
        })
    }

    pub fn resolution(
        &self,
        endpoint_key: &str,
    ) -> Result<Option<EndpointResolutionRecord>, DbError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT endpoint_key, url, classification, resolved_addresses, last_error, resolved_at \
                 FROM state_local_endpoint_resolutions WHERE endpoint_key = ?1",
            )?;
            let mut rows = stmt.query(params![endpoint_key])?;
            let Some(row) = rows.next()? else { return Ok(None); };
            let encoded: String = row.get(3)?;
            let resolved_addresses = serde_json::from_str(&encoded)
                .map_err(|error| DbError::Serde(error.to_string()))?;
            Ok(Some(EndpointResolutionRecord {
                endpoint_key: row.get(0)?,
                url: row.get(1)?,
                classification: row.get(2)?,
                resolved_addresses,
                last_error: row.get(4)?,
                resolved_at: row.get(5)?,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    #[test]
    fn approvals_and_resolution_round_trip() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = LocalEndpointPolicyStore::new(&db);
        store
            .approve(EndpointApprovalKind::Cidr, "10.2.0.0/16", 1)
            .unwrap();
        store
            .approve(EndpointApprovalKind::DnsName, "model.home.arpa", 2)
            .unwrap();
        let approvals = store.approvals().unwrap();
        assert_eq!(approvals.cidrs, ["10.2.0.0/16"]);
        assert_eq!(approvals.dns_names, ["model.home.arpa"]);

        let record = EndpointResolutionRecord {
            endpoint_key: "inference:standard".into(),
            url: "http://model.home.arpa:8000/v1".into(),
            classification: "approved_dns".into(),
            resolved_addresses: vec!["10.2.4.8".into()],
            last_error: None,
            resolved_at: 3,
        };
        store.record_resolution(&record).unwrap();
        assert_eq!(
            store.resolution("inference:standard").unwrap(),
            Some(record)
        );
    }
}
