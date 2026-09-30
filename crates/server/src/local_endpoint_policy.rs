//! Adapter between SQLite policy configuration and network enforcement.

use execlaw_core::Database;
use execlaw_core::local_endpoint_policy::{
    EndpointApprovalScope, EndpointResolutionRecord, LocalEndpointPolicyStore,
};
use execlaw_inference_api::InferenceClient;
use execlaw_local_endpoint_policy::{
    EndpointResolution, LocalEndpointPolicy, PolicyError, PublicEgressPolicy, Resolver,
};
use std::net::{SocketAddr, ToSocketAddrs};

struct FixedResolver(Vec<SocketAddr>);

impl Resolver for FixedResolver {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<SocketAddr>> {
        Ok(self.0.clone())
    }
}

/// Authorize a script-plugin HTTP destination and return addresses to pin.
/// Public destinations use public-egress rules; private destinations require
/// an operator-approved DNS name/CIDR. Loopback is reserved for sidecar RPC.
pub fn resolve_plugin_http_target(
    db: &Database,
    host_port: &str,
) -> Result<Vec<SocketAddr>, String> {
    let url = plugin_target_url(host_port)?;
    let host = url.host_str().ok_or("HTTP target has no host")?;
    let port = url
        .port_or_known_default()
        .ok_or("HTTP target has no port")?;
    let resolved = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("resolve HTTP host {host:?}: {error}"))?
        .collect::<Vec<_>>();
    check_plugin_http_addresses(db, &url, resolved)
}

fn plugin_target_url(host_port: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(&format!("http://{host_port}/"))
        .map_err(|error| format!("invalid HTTP host:port: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("HTTP target must not contain userinfo".into());
    }
    Ok(url)
}

fn check_plugin_http_addresses(
    db: &Database,
    url: &url::Url,
    resolved: Vec<SocketAddr>,
) -> Result<Vec<SocketAddr>, String> {
    let host = url.host_str().ok_or("HTTP target has no host")?;
    let port = url
        .port_or_known_default()
        .ok_or("HTTP target has no port")?;
    if resolved.is_empty() {
        return Err(format!("HTTP host {host:?} resolved to no addresses"));
    }
    if resolved
        .iter()
        .any(|socket| execlaw_local_endpoint_policy::normalize_mapped_ip(socket.ip()).is_loopback())
    {
        return Err("loopback access is reserved for registered sidecars".into());
    }
    let fixed = FixedResolver(resolved);
    let public_policy = PublicEgressPolicy;
    if let Ok(public) = public_policy.resolve_with(url.as_str(), &fixed) {
        return Ok(public
            .addresses
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect());
    }

    let local_policy = load_for_scope(db, EndpointApprovalScope::PrivateIntegration)?;
    let local = local_policy
        .validate_with(url.as_str(), &fixed)
        .map_err(|error| error.to_string())?;
    Ok(local
        .addresses
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect())
}

pub fn load(db: &Database) -> Result<LocalEndpointPolicy, String> {
    load_for_scope(db, EndpointApprovalScope::PrivateIntegration)
}

pub fn load_for_scope(
    db: &Database,
    scope: EndpointApprovalScope,
) -> Result<LocalEndpointPolicy, String> {
    let approvals = LocalEndpointPolicyStore::new(db)
        .approvals_for(scope)
        .map_err(|error| format!("read local endpoint approvals: {error}"))?;
    LocalEndpointPolicy::new(&approvals.cidrs, &approvals.dns_names)
        .map_err(|error| error.to_string())
}

pub fn checked_client(
    db: &Database,
    endpoint_key: &str,
    url: &str,
    configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
) -> Result<(reqwest::Client, EndpointResolution), String> {
    checked_client_for_scope(
        db,
        EndpointApprovalScope::PrivateIntegration,
        endpoint_key,
        url,
        configure,
    )
}

pub fn checked_client_for_scope(
    db: &Database,
    scope: EndpointApprovalScope,
    endpoint_key: &str,
    url: &str,
    configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
) -> Result<(reqwest::Client, EndpointResolution), String> {
    let policy = load_for_scope(db, scope)?;
    let now = chrono::Utc::now().timestamp();
    match policy.validate(url) {
        Ok(resolution) => {
            let client = policy
                .reqwest_client(&resolution, configure)
                .map_err(|error| error.to_string())?;
            let record = EndpointResolutionRecord {
                endpoint_key: endpoint_key.to_owned(),
                url: resolution.url.as_str().to_owned(),
                classification: resolution.classification.as_str().to_owned(),
                resolved_addresses: resolution
                    .addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                last_error: None,
                resolved_at: now,
            };
            LocalEndpointPolicyStore::new(db)
                .record_resolution(&record)
                .map_err(|error| format!("persist endpoint resolution: {error}"))?;
            Ok((client, resolution))
        }
        Err(error) => {
            let record = EndpointResolutionRecord {
                endpoint_key: endpoint_key.to_owned(),
                url: url.to_owned(),
                classification: "denied".to_owned(),
                resolved_addresses: denied_address(&error)
                    .into_iter()
                    .map(|address| address.to_string())
                    .collect(),
                last_error: Some(error.to_string()),
                resolved_at: now,
            };
            let _ = LocalEndpointPolicyStore::new(db).record_resolution(&record);
            Err(error.to_string())
        }
    }
}

/// Build a direct client for a public web/search endpoint after resolving and
/// pinning every DNS answer. The client does not use ambient proxies or follow
/// redirects; call sites must validate any redirect target as a new request.
pub fn checked_public_client(
    url: &str,
    configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
) -> Result<reqwest::Client, String> {
    let policy = PublicEgressPolicy;
    let resolution = policy
        .resolve(url)
        .map_err(|error| format!("public endpoint denied: {error}"))?;
    policy
        .reqwest_client(&resolution, configure)
        .map_err(|error| format!("build public endpoint client: {error}"))
}

fn denied_address(error: &PolicyError) -> Option<std::net::IpAddr> {
    match error {
        PolicyError::UnapprovedAddress(address) => Some(*address),
        _ => None,
    }
}

pub fn checked_inference_client(
    db: &Database,
    endpoint_key: &str,
    url: &str,
) -> Result<InferenceClient, String> {
    let policy = load_for_scope(db, EndpointApprovalScope::LocalInference)?;
    let now = chrono::Utc::now().timestamp();
    match InferenceClient::new_with_policy(url.to_owned(), &policy) {
        Ok(client) => {
            let resolution = client
                .endpoint_resolution()
                .expect("checked client has resolution");
            LocalEndpointPolicyStore::new(db)
                .record_resolution(&EndpointResolutionRecord {
                    endpoint_key: endpoint_key.to_owned(),
                    url: resolution.url.as_str().to_owned(),
                    classification: resolution.classification.as_str().to_owned(),
                    resolved_addresses: resolution
                        .addresses
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    last_error: None,
                    resolved_at: now,
                })
                .map_err(|error| format!("persist endpoint resolution: {error}"))?;
            Ok(client)
        }
        Err(error) => {
            let _ =
                LocalEndpointPolicyStore::new(db).record_resolution(&EndpointResolutionRecord {
                    endpoint_key: endpoint_key.to_owned(),
                    url: url.to_owned(),
                    classification: "denied".to_owned(),
                    resolved_addresses: denied_address(&error)
                        .into_iter()
                        .map(|address| address.to_string())
                        .collect(),
                    last_error: Some(error.to_string()),
                    resolved_at: now,
                });
            Err(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::db::DbConfig;
    use execlaw_core::local_endpoint_policy::EndpointApprovalKind;
    use execlaw_core::migrations::MigrationRunner;

    #[test]
    fn plugin_http_denies_mixed_dns_and_preserves_approved_private_endpoint() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = LocalEndpointPolicyStore::new(&db);
        store
            .approve_for(
                EndpointApprovalScope::PrivateIntegration,
                EndpointApprovalKind::DnsName,
                "plugin.lan",
                1,
            )
            .unwrap();
        store
            .approve_for(
                EndpointApprovalScope::PrivateIntegration,
                EndpointApprovalKind::Cidr,
                "10.20.0.0/16",
                1,
            )
            .unwrap();
        let url = plugin_target_url("plugin.lan:8080").unwrap();
        let private = "10.20.1.2:8080".parse().unwrap();
        assert_eq!(
            check_plugin_http_addresses(&db, &url, vec![private]).unwrap(),
            vec![private]
        );
        for additional in [
            "8.8.8.8:8080",
            "169.254.169.254:8080",
            "[::ffff:8.8.8.8]:8080",
        ] {
            assert!(
                check_plugin_http_addresses(&db, &url, vec![private, additional.parse().unwrap()],)
                    .is_err(),
                "mixed DNS answer {additional} must deny the plugin request"
            );
        }
        assert!(
            check_plugin_http_addresses(
                &db,
                &plugin_target_url("plugin.lan:8080").unwrap(),
                vec!["127.0.0.1:8080".parse().unwrap()],
            )
            .is_err()
        );
        assert!(
            check_plugin_http_addresses(
                &db,
                &plugin_target_url("plugin.lan:8080").unwrap(),
                vec!["[::ffff:127.0.0.1]:8080".parse().unwrap()],
            )
            .is_err()
        );
    }
}
