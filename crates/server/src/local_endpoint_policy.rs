//! Adapter between SQLite policy configuration and network enforcement.

use execlaw_core::Database;
use execlaw_core::local_endpoint_policy::{EndpointResolutionRecord, LocalEndpointPolicyStore};
use execlaw_inference_api::InferenceClient;
use execlaw_local_endpoint_policy::{EndpointResolution, LocalEndpointPolicy, PolicyError};

pub fn load(db: &Database) -> Result<LocalEndpointPolicy, String> {
    let approvals = LocalEndpointPolicyStore::new(db)
        .approvals()
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
    let policy = load(db)?;
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
    let policy = load(db)?;
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
