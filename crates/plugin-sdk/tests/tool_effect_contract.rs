use execlaw_plugin_sdk::{
    PluginManifest, ToolCancellation, ToolConcurrency, ToolEffectContract, ToolEffectPolicy,
    ToolExternalEffect, ToolIdempotency, ToolReconciliation, ToolResourceAccess, ToolResourceMode,
    ToolSensitivity,
};

#[test]
fn unknown_tool_effects_fail_closed_for_retries_and_parallelism() {
    let contract = ToolEffectContract::default();
    let operator_policy = ToolEffectPolicy::default();
    assert!(!contract.allows_automatic_effect_retry(&operator_policy));
    assert!(!contract.allows_parallel_execution(&operator_policy));
    let incomplete_write = ToolEffectContract {
        idempotency: ToolIdempotency::FrameworkKey,
        reconciliation: ToolReconciliation::IdempotencyLookup,
        ..ToolEffectContract::default()
    };
    assert!(
        !incomplete_write.allows_automatic_effect_retry(&ToolEffectPolicy {
            allow_automatic_retries: true,
            allow_parallel_execution: true,
        })
    );
}

#[test]
fn old_manifests_normalize_omitted_contracts_conservatively() {
    let manifest = PluginManifest::parse(
        r#"
[plugin]
id = "fixture"
name = "Fixture"
version = "1.0.0"

[[tools]]
name = "read"
required_capabilities = []
"#,
    )
    .unwrap();
    let normalized = manifest.tools[0].normalized_effect_contract();
    assert_eq!(normalized.external_effect, ToolExternalEffect::Unknown);
    let operator_policy = ToolEffectPolicy {
        allow_automatic_retries: true,
        allow_parallel_execution: true,
    };
    assert!(!normalized.allows_automatic_effect_retry(&operator_policy));
    assert!(!normalized.allows_parallel_execution(&operator_policy));
}

#[test]
fn declared_effect_contract_roundtrips_for_all_runtime_tiers() {
    let contract = ToolEffectContract {
        resources: vec![ToolResourceAccess {
            resource: "calendar:event".into(),
            access: ToolResourceMode::Write,
        }],
        external_effect: ToolExternalEffect::ExternalWrite,
        idempotency: ToolIdempotency::FrameworkKey,
        reconciliation: ToolReconciliation::IdempotencyLookup,
        cancellation: ToolCancellation::BestEffort,
        sensitivity: ToolSensitivity::Personal,
        concurrency: ToolConcurrency::Keyed,
        dependencies: vec!["calendar.lookup".into()],
    };
    let encoded = toml::to_string(&contract).unwrap();
    let decoded: ToolEffectContract = toml::from_str(&encoded).unwrap();
    assert_eq!(decoded, contract);
    let operator_policy = ToolEffectPolicy {
        allow_automatic_retries: true,
        allow_parallel_execution: true,
    };
    assert!(decoded.allows_automatic_effect_retry(&operator_policy));
    assert!(!decoded.allows_parallel_execution(&operator_policy));
}

#[test]
fn declared_read_dependency_is_never_parallelized_with_its_round() {
    let contract = ToolEffectContract {
        resources: vec![ToolResourceAccess {
            resource: "calendar.events".into(),
            access: ToolResourceMode::Read,
        }],
        external_effect: ToolExternalEffect::ReadOnly,
        concurrency: ToolConcurrency::ReadOnly,
        dependencies: vec!["calendar.list_calendars".into()],
        ..ToolEffectContract::default()
    };
    assert!(!contract.allows_parallel_execution(&ToolEffectPolicy {
        allow_automatic_retries: false,
        allow_parallel_execution: true,
    }));
}

#[test]
fn google_apps_parallel_reads_are_declared_and_writes_stay_serial() {
    let manifest =
        PluginManifest::parse(include_str!("../../../plugins/google-apps/plugin.toml")).unwrap();
    let policy = ToolEffectPolicy {
        allow_automatic_retries: false,
        allow_parallel_execution: true,
    };
    let search = manifest
        .tools
        .iter()
        .find(|tool| tool.name == "gmail.search")
        .unwrap()
        .normalized_effect_contract();
    let send = manifest
        .tools
        .iter()
        .find(|tool| tool.name == "gmail.send_message")
        .unwrap()
        .normalized_effect_contract();
    assert!(search.allows_parallel_execution(&policy));
    assert!(!send.allows_parallel_execution(&policy));
}
