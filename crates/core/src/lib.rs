//! execlaw-core
//!
//! The durability heart of execlaw. See `README.md` and MIGRATION_PLAN.md §2
//! for the design.
//!
//! Keep this crate free of network I/O and vendor SDKs. It owns:
//!
//! - SQLite connection pool + SQLCipher key loading
//! - Schema migration runner
//! - Event log primitives
//! - Conversation / principal / outbox / alert data models
//!
//! Anything that talks to Docker, the inference backend, or a transport
//! belongs in a sibling crate.

#![forbid(unsafe_code)]

pub mod agent_contract;
pub mod agent_ownership;
pub mod agents;
pub mod alerts;
pub mod artifact_provenance;
pub mod artifact_sweeper;
pub mod attachments;
pub mod audit;
pub mod automation_bus;
pub mod automation_runs;
pub mod automation_suggestions;
pub mod automations;
pub mod backends;
pub mod builtin_tools;
pub mod bus_event_retention;
pub mod cards;
pub mod chat_requests;
mod completion_evidence;
pub mod config;
pub mod conversation;
pub mod db;
pub mod entities;
pub mod ephemeral_sweeper;
pub mod eval;
pub mod event_hmac;
pub mod event_retention;
pub mod events;
pub mod general_settings;
pub mod graphiti;
pub mod harness;
pub mod history_budget;
pub mod ids;
pub mod information;
pub mod information_store;
pub(crate) mod job_redrive;
pub mod local_endpoint_policy;
pub mod log_retention;
pub mod logs;
pub mod mcp_servers;
pub mod memory;
pub mod memory_assertions;
pub mod memory_assets;
pub mod memory_lifecycle;
pub mod message_archive;
pub mod migrations;
pub mod oauth;
pub mod outbox;
pub mod personality;
pub mod policy_simulation;
pub mod preferences;
pub mod principal;
pub mod principal_groups;
pub mod process_environment;
pub mod projection_rebuild;
pub mod qualified_read_cache;
pub mod refresh_tokens;
pub mod reply_drafts;
pub mod research;
pub mod resource_versions;
pub mod retention;
pub mod routine_run_retention;
pub mod routines;
pub mod runs;
pub mod safety_profiles;
pub mod search_providers;
pub mod skills_config;
pub mod tool;
pub mod tool_access;
pub mod tool_apis;
pub mod tool_execution;
pub mod transport_bindings;
pub mod transport_conversations;
pub mod transport_cursor;
pub mod trust_policy;
pub mod turn_controls;
pub mod users;
pub mod vault_row;
pub mod webauthn;
pub mod workspaces;

pub use db::{Database, DbConfig, DbError};
pub use events::{
    EventKind, EventLog, EventRecord, FreshnessComparison, FreshnessHead, FreshnessManifest,
};
pub use ids::{
    AlertId, AttachmentId, ConversationId, EventSeq, IdempotencyKey, IncidentId, PluginId,
    PrincipalId, ResearchJobId, TurnSeq,
};
pub use migrations::{MigrationError, MigrationRunner};
