$ErrorActionPreference = "Stop"

$cases = @(
    @{ Package = "execlaw-local-endpoint-policy"; Filter = "tests::" },
    @{ Package = "execlaw-server"; Filter = "http_fetch_public_egress_rejects_loopback_before_connecting" },
    @{ Package = "execlaw-server"; Filter = "oauth_token_redirect_does_not_forward_credentials" },
    @{ Package = "execlaw-server"; Filter = "production_oauth_denies_private_destination_before_a_request" },
    @{ Package = "execlaw-server"; Filter = "denied_private_http_endpoint_waits_for_a_later_policy_grant" },
    @{ Package = "execlaw-script"; Filter = "sidecar_http_agent_does_not_follow_a_cross_sidecar_redirect" },
    @{ Package = "execlaw-server"; Filter = "plugin_http_denies_mixed_dns_and_preserves_approved_private_endpoint" },
    @{ Package = "execlaw-server"; Filter = "configured_public_endpoint_is_denied_before_request" },
    @{ Package = "execlaw-policy"; Filter = "input_guard::tests" },
    @{ Package = "execlaw-policy"; Filter = "spotlighting::tests" },
    @{ Package = "execlaw-policy"; Filter = "trust::tests" },
    @{ Package = "execlaw-runner-local"; Filter = "malformed_tool_arguments_are_rejected_without_dispatch" },
    @{ Package = "execlaw-runner-local"; Filter = "supported_model_tool_call_fixtures_obey_advertised_schema" },
    @{ Package = "execlaw-server"; Filter = "tool_apis_http::tests" },
    @{ Package = "execlaw-server"; Filter = "search_is_conversation_scoped_and_fails_closed_on_hmac_tampering" },
    @{ Package = "execlaw-server"; Filter = "build_runner_tool_catalog_strips_all_tools_when_planner_executor" }
)

foreach ($case in $cases) {
    Write-Host "Running adversarial fixture: $($case.Package) $($case.Filter)"
    cargo test -p $case.Package --lib $case.Filter
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
