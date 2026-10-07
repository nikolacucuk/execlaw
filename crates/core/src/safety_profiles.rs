//! Task-scoped safety profiles and immutable run snapshots.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// One of the built-in task safety profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafetyProfileId {
    InspectOnly,
    WorkspaceEdit,
    ApprovedIntegration,
}

impl SafetyProfileId {
    /// Stable SQLite and API representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InspectOnly => "inspect_only",
            Self::WorkspaceEdit => "workspace_edit",
            Self::ApprovedIntegration => "approved_integration",
        }
    }

    /// Parse a stable profile identifier.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "inspect_only" => Some(Self::InspectOnly),
            "workspace_edit" => Some(Self::WorkspaceEdit),
            "approved_integration" => Some(Self::ApprovedIntegration),
            _ => None,
        }
    }

    /// Human-readable label shown before a run starts.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::InspectOnly => "Inspect only",
            Self::WorkspaceEdit => "Workspace edit",
            Self::ApprovedIntegration => "Approved integrations",
        }
    }
}

/// Permission categories enforced independently of model instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SafetyCapabilities {
    pub data_read: bool,
    pub workspace_read: bool,
    pub workspace_write: bool,
    pub workspace_process: bool,
    pub approved_integration: bool,
    pub approved_network: bool,
    pub brokered_secret_use: bool,
    pub approved_destinations: bool,
}

impl SafetyCapabilities {
    fn for_profile(id: SafetyProfileId) -> Self {
        match id {
            SafetyProfileId::InspectOnly => Self {
                data_read: true,
                workspace_read: true,
                workspace_write: false,
                workspace_process: false,
                approved_integration: false,
                approved_network: false,
                brokered_secret_use: false,
                approved_destinations: false,
            },
            SafetyProfileId::WorkspaceEdit => Self {
                data_read: true,
                workspace_read: true,
                workspace_write: true,
                workspace_process: true,
                approved_integration: false,
                approved_network: false,
                brokered_secret_use: false,
                approved_destinations: false,
            },
            SafetyProfileId::ApprovedIntegration => Self {
                data_read: true,
                workspace_read: true,
                workspace_write: false,
                workspace_process: false,
                approved_integration: true,
                approved_network: true,
                brokered_secret_use: true,
                approved_destinations: true,
            },
        }
    }

    /// Whether a declared plugin capability is allowed by this profile.
    pub fn allows_declared_capability(&self, capability: &str) -> bool {
        match capability {
            "workspace.read" => self.workspace_read,
            "workspace.write" => self.workspace_write,
            "workspace.process" => self.workspace_process,
            "integration.approved" => self.approved_integration,
            "network.approved" => self.approved_network,
            "secret.brokered" => self.brokered_secret_use,
            "destination.approved" => self.approved_destinations,
            "data.read" => self.data_read,
            _ => false,
        }
    }

    /// Whether a built-in capability is available under this profile.
    pub fn allows_builtin(
        &self,
        capability: crate::tool::Capability,
        explicitly_approved: bool,
    ) -> bool {
        use crate::tool::Capability;
        match capability {
            Capability::ConversationRead
            | Capability::TaskRead
            | Capability::ScheduleRead
            | Capability::ResearchRead => self.data_read,
            Capability::MemoryRead => self.brokered_secret_use && explicitly_approved,
            Capability::WebFetch | Capability::Search | Capability::ResearchSpawn => {
                self.approved_network && explicitly_approved
            }
            Capability::Transport | Capability::Notify | Capability::AttachmentSend => {
                self.approved_destinations && explicitly_approved
            }
            Capability::McpAdmin | Capability::SubagentSpawn => {
                self.approved_integration && explicitly_approved
            }
            Capability::ConversationWrite
            | Capability::MemoryWrite
            | Capability::TaskWrite
            | Capability::ScheduleWrite => false,
        }
    }
}

/// Configured profile as shown to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SafetyProfile {
    pub profile_id: SafetyProfileId,
    pub display_name: String,
    pub capabilities: SafetyCapabilities,
    pub approved_tools: Vec<String>,
    pub revision: i64,
    pub updated_by: String,
    pub updated_at: i64,
}

/// Immutable effective profile attached to one durable run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SafetyProfileSnapshot {
    pub profile_id: SafetyProfileId,
    pub revision: i64,
    pub capabilities: SafetyCapabilities,
    pub approved_tools: Vec<String>,
}

impl SafetyProfileSnapshot {
    /// Freeze the currently stored profile revision for one execution.
    pub fn from_profile(profile: &SafetyProfile) -> Self {
        Self {
            profile_id: profile.profile_id,
            revision: profile.revision,
            capabilities: profile.capabilities.clone(),
            approved_tools: profile.approved_tools.clone(),
        }
    }

    /// Check plugin metadata at both catalog construction and dispatch.
    pub fn allows_plugin_tool(&self, tool_name: &str, required: &[String]) -> bool {
        let explicitly_approved = self.profile_id == SafetyProfileId::ApprovedIntegration
            && self
                .approved_tools
                .iter()
                .any(|approved| approved == tool_name);
        if required.is_empty() {
            return explicitly_approved;
        }
        required.iter().all(|capability| {
            if matches!(
                capability.as_str(),
                "workspace.read"
                    | "workspace.write"
                    | "workspace.process"
                    | "integration.approved"
                    | "network.approved"
                    | "secret.brokered"
                    | "destination.approved"
                    | "data.read"
            ) {
                self.capabilities.allows_declared_capability(capability)
            } else {
                explicitly_approved
            }
        })
    }

    /// Check a built-in descriptor at both catalog construction and dispatch.
    fn profile_tool_approved(&self, tool_name: &str) -> bool {
        self.approved_tools
            .iter()
            .any(|approved| approved == tool_name)
    }

    /// Whether a built-in capability is permitted by this exact run profile.
    pub fn allows_builtin(
        &self,
        tool_name: &str,
        capabilities: &[crate::tool::Capability],
    ) -> bool {
        !capabilities.is_empty()
            && capabilities.iter().all(|capability| {
                self.capabilities
                    .allows_builtin(*capability, self.profile_tool_approved(tool_name))
            })
    }

    /// Check an MCP tool. MCP tools are approved by their exact reflected name.
    pub fn allows_mcp_tool(&self, tool_name: &str) -> bool {
        self.profile_id == SafetyProfileId::ApprovedIntegration
            && self.capabilities.approved_integration
            && self
                .approved_tools
                .iter()
                .any(|approved| approved == tool_name)
    }
}

/// Safety-profile persistence errors.
#[derive(Debug, Error)]
pub enum SafetyProfileError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("unknown safety profile '{0}'")]
    NotFound(String),
    #[error("invalid safety profile: {0}")]
    Invalid(String),
    #[error("safety profile snapshot conflicts with the run's saved inputs")]
    Conflict,
}

/// SQLite access for the built-in safety profiles and per-run snapshots.
pub struct SafetyProfileStore<'db> {
    db: &'db Database,
}

impl<'db> SafetyProfileStore<'db> {
    /// Create a profile store for an open database.
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// List the operator-visible profiles in stable name order.
    pub fn list(&self) -> Result<Vec<SafetyProfile>, SafetyProfileError> {
        self.db
            .with_conn(|connection| {
                let mut statement = connection.prepare(
                    "SELECT profile_id, display_name, capability_set_json, approved_tools_json, \
                     revision, updated_by, updated_at FROM config_safety_profiles \
                     ORDER BY profile_id",
                )?;
                statement
                    .query_map([], row_to_profile)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbError::from)
            })
            .map_err(SafetyProfileError::from)
    }

    /// Load one profile by its stable identifier.
    pub fn get(&self, profile_id: SafetyProfileId) -> Result<SafetyProfile, SafetyProfileError> {
        let profile = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT profile_id, display_name, capability_set_json, approved_tools_json, \
                         revision, updated_by, updated_at FROM config_safety_profiles \
                         WHERE profile_id = ?1",
                    [profile_id.as_str()],
                    row_to_profile,
                )
                .optional()
                .map_err(DbError::from)
        })?;
        profile.ok_or_else(|| SafetyProfileError::NotFound(profile_id.as_str().to_owned()))
    }

    /// Replace the exact approved integration tool names and create a new revision.
    pub fn set_approved_tools(
        &self,
        profile_id: SafetyProfileId,
        tool_names: &[String],
        actor: &str,
        now: i64,
    ) -> Result<SafetyProfile, SafetyProfileError> {
        if actor.trim().is_empty() || actor.len() > 128 {
            return Err(SafetyProfileError::Invalid(
                "invalid Controller identity".into(),
            ));
        }
        if profile_id != SafetyProfileId::ApprovedIntegration {
            return Err(SafetyProfileError::Invalid(
                "only the approved-integration profile has a destination list".into(),
            ));
        }
        let mut unique = BTreeSet::new();
        for tool in tool_names {
            if tool.trim().is_empty() || tool.len() > 240 || !unique.insert(tool.as_str()) {
                return Err(SafetyProfileError::Invalid(
                    "approved tools must be unique registered tool names of 1 to 240 bytes".into(),
                ));
            }
        }
        let approved_json = serde_json::to_string(&unique.into_iter().collect::<Vec<_>>())
            .map_err(|error| SafetyProfileError::Invalid(error.to_string()))?;
        self.db.transaction(|tx| {
            let capability_json: String = tx.query_row(
                "SELECT capability_set_json FROM config_safety_profiles WHERE profile_id = ?1",
                [profile_id.as_str()],
                |row| row.get(0),
            )?;
            let revision: i64 = tx.query_row(
                "SELECT revision FROM config_safety_profiles WHERE profile_id = ?1",
                [profile_id.as_str()],
                |row| row.get(0),
            )?;
            let next = revision + 1;
            tx.execute(
                "UPDATE config_safety_profiles SET approved_tools_json = ?1, revision = ?2, \
                 updated_by = ?3, updated_at = ?4 WHERE profile_id = ?5",
                params![approved_json, next, actor, now, profile_id.as_str()],
            )?;
            tx.execute(
                "INSERT INTO config_safety_profile_revisions \
                 (profile_id, revision, capability_set_json, approved_tools_json, saved_by, saved_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![profile_id.as_str(), next, capability_json, approved_json, actor, now],
            )?;
            Ok(())
        })?;
        self.get(profile_id)
    }

    /// Read an immutable profile revision referenced by a user-message event.
    pub fn get_revision(
        &self,
        profile_id: SafetyProfileId,
        revision: i64,
    ) -> Result<SafetyProfileSnapshot, SafetyProfileError> {
        let row = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT capability_set_json, approved_tools_json \
                     FROM config_safety_profile_revisions WHERE profile_id = ?1 AND revision = ?2",
                    params![profile_id.as_str(), revision],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(DbError::from)
        })?;
        let Some((capabilities_json, approved_tools_json)) = row else {
            return Err(SafetyProfileError::NotFound(format!(
                "{} revision {revision}",
                profile_id.as_str()
            )));
        };
        let capability_names: Vec<String> = serde_json::from_str(&capabilities_json)
            .map_err(|error| SafetyProfileError::Invalid(error.to_string()))?;
        let capabilities = SafetyCapabilities::for_profile(profile_id);
        let expected: BTreeSet<_> = profile_capability_names(&capabilities)
            .into_iter()
            .collect();
        if capability_names.into_iter().collect::<BTreeSet<_>>() != expected {
            return Err(SafetyProfileError::Invalid(
                "stored profile capability revision is invalid".into(),
            ));
        }
        let approved_tools: Vec<String> = serde_json::from_str(&approved_tools_json)
            .map_err(|error| SafetyProfileError::Invalid(error.to_string()))?;
        Ok(SafetyProfileSnapshot {
            profile_id,
            revision,
            capabilities,
            approved_tools,
        })
    }
}

fn row_to_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<SafetyProfile> {
    let id: String = row.get(0)?;
    let profile_id = SafetyProfileId::parse(&id).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown safety profile",
            )),
        )
    })?;
    let serialized_caps: String = row.get(2)?;
    let capability_names: Vec<String> =
        serde_json::from_str(&serialized_caps).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    let capabilities = SafetyCapabilities::for_profile(profile_id);
    let expected: BTreeSet<String> = profile_capability_names(&capabilities)
        .into_iter()
        .collect();
    let stored: BTreeSet<_> = capability_names.into_iter().collect();
    if stored != expected {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "invalid safety capability set for {id}: {stored:?}; expected {expected:?}"
                ),
            )),
        ));
    }
    let approved_json: String = row.get(3)?;
    let approved_tools: Vec<String> = serde_json::from_str(&approved_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(SafetyProfile {
        profile_id,
        display_name: row.get(1)?,
        capabilities,
        approved_tools,
        revision: row.get(4)?,
        updated_by: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn profile_capability_names(capabilities: &SafetyCapabilities) -> Vec<String> {
    let mut names = Vec::new();
    for (allowed, name) in [
        (capabilities.data_read, "data.read"),
        (capabilities.workspace_read, "workspace.read"),
        (capabilities.workspace_write, "workspace.write"),
        (capabilities.workspace_process, "workspace.process"),
        (capabilities.approved_integration, "integration.approved"),
        (capabilities.approved_network, "network.approved"),
        (capabilities.brokered_secret_use, "secret.brokered"),
        (capabilities.approved_destinations, "destination.approved"),
    ] {
        if allowed {
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::MigrationRunner;

    fn fresh_db() -> Database {
        let db = Database::open(&crate::db::DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn profiles_expose_distinct_database_backed_capability_sets() {
        let db = fresh_db();
        let store = SafetyProfileStore::new(&db);
        let profiles = store.list().unwrap();
        assert_eq!(profiles.len(), 3);
        let inspect = store.get(SafetyProfileId::InspectOnly).unwrap();
        assert!(!inspect.capabilities.workspace_write);
        assert!(!inspect.capabilities.approved_network);
        let edit = store.get(SafetyProfileId::WorkspaceEdit).unwrap();
        assert!(edit.capabilities.workspace_write);
        assert!(edit.capabilities.workspace_process);
        assert!(!edit.capabilities.approved_destinations);
        let integration = store.get(SafetyProfileId::ApprovedIntegration).unwrap();
        assert!(integration.capabilities.approved_network);
        assert!(integration.capabilities.brokered_secret_use);
        assert!(integration.capabilities.approved_destinations);
    }

    #[test]
    fn profile_snapshots_are_immutable_across_revision_changes() {
        let db = fresh_db();
        let store = SafetyProfileStore::new(&db);
        let before = store.get(SafetyProfileId::ApprovedIntegration).unwrap();
        let snapshot = SafetyProfileSnapshot::from_profile(&before);
        store
            .set_approved_tools(
                SafetyProfileId::ApprovedIntegration,
                &["calendar.create_event".into()],
                "controller-1",
                20,
            )
            .unwrap();
        assert_eq!(
            store
                .get_revision(snapshot.profile_id, snapshot.revision)
                .unwrap(),
            snapshot
        );
        assert_eq!(
            store
                .get(SafetyProfileId::ApprovedIntegration)
                .unwrap()
                .revision,
            2
        );
    }

    #[test]
    fn task_profiles_do_not_expand_trust_or_implicit_plugin_access() {
        let db = fresh_db();
        let store = SafetyProfileStore::new(&db);
        let inspect = store.get(SafetyProfileId::InspectOnly).unwrap();
        let snapshot = SafetyProfileSnapshot {
            profile_id: inspect.profile_id,
            revision: inspect.revision,
            capabilities: inspect.capabilities,
            approved_tools: inspect.approved_tools,
        };
        assert!(snapshot.allows_plugin_tool("workspace.read_file", &["workspace.read".into()]));
        assert!(!snapshot.allows_plugin_tool("workspace.apply_patch", &["workspace.write".into()]));
        assert!(!snapshot.allows_plugin_tool("unknown.plugin_tool", &[]));
        assert!(!snapshot.allows_builtin("transport.send", &[crate::tool::Capability::Transport]));

        let approved = store.get(SafetyProfileId::ApprovedIntegration).unwrap();
        let mut approved_snapshot = SafetyProfileSnapshot::from_profile(&approved);
        approved_snapshot.approved_tools = vec!["calendar.create".into()];
        assert!(
            approved_snapshot.allows_plugin_tool("calendar.create", &["network.approved".into()])
        );
        assert!(
            !approved_snapshot.allows_plugin_tool("calendar.create", &["workspace.write".into()])
        );
    }
}
