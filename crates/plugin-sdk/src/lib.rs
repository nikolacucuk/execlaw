//! execlaw-plugin-sdk
//!
//! Parses `plugin.toml` manifests with the **hook-declaration** model from
//! §4.2 — not a typed `kind`. A plugin declares which hook points it
//! attaches to (`tools`, `transport`, `identity_provider`,
//! `inference_backend`, `service`, `oauth_accounts`, `ui_panels`,
//! `chat_components`, `event_subscriptions`, `alert_sources`,
//! `health_checks`, `hardware_probe`, `skills`). A single plugin may
//! attach to many hooks.
//!
//! This aligns with the 2026-04-23 locked decision: "Every plugin is a
//! plugin. No typed plugin kinds."

#![forbid(unsafe_code)]

pub mod manifest;
pub mod zip_stage;

pub use manifest::{
    DeprecatedPrimitiveUse, HOST_API_FEATURES, HOST_API_VERSION, HealthCheckProbe,
    OauthAccountDecl, PluginCompatibility, PluginHeader, PluginManifest, RuntimeDecl,
    ToolCancellation, ToolConcurrency, ToolDecl, ToolEffectContract, ToolEffectPolicy,
    ToolExternalEffect, ToolIdempotency, ToolReconciliation, ToolResourceAccess, ToolResourceMode,
    ToolSensitivity, UiPanelDecl,
};
pub use zip_stage::{StageError, StagedPlugin, stage_zip};
