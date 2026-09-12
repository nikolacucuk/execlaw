//! Local-only endpoint validation and DNS-rebinding-safe reqwest clients.

#![forbid(unsafe_code)]

use ipnet::IpNet;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use thiserror::Error;
use url::{Host, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointClassification {
    Loopback,
    ApprovedCidr,
    ApprovedDns,
}

impl EndpointClassification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::ApprovedCidr => "approved_cidr",
            Self::ApprovedDns => "approved_dns",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointResolution {
    pub url: Url,
    pub classification: EndpointClassification,
    pub addresses: Vec<IpAddr>,
}

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("invalid endpoint URL: {0}")]
    InvalidUrl(String),
    #[error("endpoint scheme must be http or https")]
    Scheme,
    #[error("endpoint URL must not contain userinfo")]
    Userinfo,
    #[error("alternate numeric host forms are not allowed")]
    AlternateNumericHost,
    #[error("DNS name {0:?} is not operator-approved")]
    UnapprovedDns(String),
    #[error("address {0} is neither loopback nor in an operator-approved CIDR")]
    UnapprovedAddress(IpAddr),
    #[error("DNS resolution for {0:?} returned no addresses")]
    EmptyResolution(String),
    #[error("DNS resolution failed for {host:?}: {message}")]
    Resolution { host: String, message: String },
    #[error("invalid approved CIDR {0:?}")]
    InvalidCidr(String),
    #[error("HTTP client build failed: {0}")]
    Client(String),
}

pub trait Resolver: Send + Sync {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        (host, port).to_socket_addrs().map(|items| items.collect())
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalEndpointPolicy {
    approved_cidrs: Vec<IpNet>,
    approved_dns_names: BTreeSet<String>,
}

impl LocalEndpointPolicy {
    pub fn new<C, D>(cidrs: C, dns_names: D) -> Result<Self, PolicyError>
    where
        C: IntoIterator,
        C::Item: AsRef<str>,
        D: IntoIterator,
        D::Item: AsRef<str>,
    {
        let approved_cidrs = cidrs
            .into_iter()
            .map(|cidr| {
                cidr.as_ref()
                    .parse::<IpNet>()
                    .map_err(|_| PolicyError::InvalidCidr(cidr.as_ref().to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let approved_dns_names = dns_names
            .into_iter()
            .map(|name| name.as_ref().trim_end_matches('.').to_ascii_lowercase())
            .filter(|name| !name.is_empty())
            .collect();
        Ok(Self {
            approved_cidrs,
            approved_dns_names,
        })
    }

    pub fn loopback_only() -> Self {
        Self::default()
    }

    pub fn validate(&self, raw_url: &str) -> Result<EndpointResolution, PolicyError> {
        self.validate_with(raw_url, &SystemResolver)
    }

    pub fn validate_with(
        &self,
        raw_url: &str,
        resolver: &dyn Resolver,
    ) -> Result<EndpointResolution, PolicyError> {
        reject_alternate_numeric_host(raw_url)?;
        let url =
            Url::parse(raw_url).map_err(|error| PolicyError::InvalidUrl(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(PolicyError::Scheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PolicyError::Userinfo);
        }
        let host = url
            .host()
            .ok_or_else(|| PolicyError::InvalidUrl("missing host".into()))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| PolicyError::InvalidUrl("missing port".into()))?;
        match host {
            Host::Ipv4(address) => self.validate_addresses(url, vec![IpAddr::V4(address)], false),
            Host::Ipv6(address) => self.validate_addresses(url, vec![IpAddr::V6(address)], false),
            Host::Domain(name) => {
                let normalized = name.trim_end_matches('.').to_ascii_lowercase();
                if normalized != "localhost" && !self.approved_dns_names.contains(&normalized) {
                    return Err(PolicyError::UnapprovedDns(normalized));
                }
                let sockets =
                    resolver
                        .resolve(name, port)
                        .map_err(|error| PolicyError::Resolution {
                            host: name.to_owned(),
                            message: error.to_string(),
                        })?;
                let addresses = sockets
                    .into_iter()
                    .map(|socket| socket.ip())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                self.validate_addresses(url, addresses, true)
            }
        }
    }

    fn validate_addresses(
        &self,
        url: Url,
        addresses: Vec<IpAddr>,
        dns: bool,
    ) -> Result<EndpointResolution, PolicyError> {
        if addresses.is_empty() {
            return Err(PolicyError::EmptyResolution(
                url.host_str().unwrap_or_default().to_owned(),
            ));
        }
        for address in &addresses {
            if !address.is_loopback()
                && !self
                    .approved_cidrs
                    .iter()
                    .any(|cidr| cidr.contains(address))
            {
                return Err(PolicyError::UnapprovedAddress(*address));
            }
        }
        let classification = if dns {
            EndpointClassification::ApprovedDns
        } else if addresses.iter().all(IpAddr::is_loopback) {
            EndpointClassification::Loopback
        } else {
            EndpointClassification::ApprovedCidr
        };
        Ok(EndpointResolution {
            url,
            classification,
            addresses,
        })
    }

    pub fn reqwest_client(
        &self,
        resolution: &EndpointResolution,
        configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    ) -> Result<reqwest::Client, PolicyError> {
        let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        if matches!(resolution.url.host(), Some(Host::Domain(_))) {
            let host = resolution.url.host_str().expect("validated host");
            let port = resolution
                .url
                .port_or_known_default()
                .expect("validated port");
            let sockets = resolution
                .addresses
                .iter()
                .map(|address| SocketAddr::new(*address, port))
                .collect::<Vec<_>>();
            builder = builder.resolve_to_addrs(host, &sockets);
        }
        configure(builder)
            .build()
            .map_err(|error| PolicyError::Client(error.to_string()))
    }
}

fn reject_alternate_numeric_host(raw_url: &str) -> Result<(), PolicyError> {
    let authority = raw_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(raw_url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, value)| value)
        .unwrap_or(authority);
    let host = if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .map(|(value, _)| format!("{value}]"))
            .unwrap_or_else(|| host_port.to_owned())
    } else {
        host_port.split(':').next().unwrap_or_default().to_owned()
    };
    let lower = host.to_ascii_lowercase();
    let dotted_leading_zero = lower.split('.').any(|part| {
        part.len() > 1 && part.starts_with('0') && part.chars().all(|ch| ch.is_ascii_digit())
    });
    let integer_form = !lower.contains('.') && lower.chars().all(|ch| ch.is_ascii_digit());
    let hex_or_octal = lower.starts_with("0x") || dotted_leading_zero;
    if integer_form || hex_or_octal {
        return Err(PolicyError::AlternateNumericHost);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeResolver {
        answers: Mutex<HashMap<String, Vec<SocketAddr>>>,
    }

    impl FakeResolver {
        fn new(host: &str, answers: &[&str]) -> Self {
            let addresses = answers
                .iter()
                .map(|address| address.parse().unwrap())
                .collect();
            Self {
                answers: Mutex::new(HashMap::from([(host.into(), addresses)])),
            }
        }
    }

    impl Resolver for FakeResolver {
        fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<SocketAddr>> {
            Ok(self
                .answers
                .lock()
                .unwrap()
                .get(host)
                .cloned()
                .unwrap_or_default())
        }
    }

    #[test]
    fn accepts_ipv4_and_ipv6_loopback() {
        let policy = LocalEndpointPolicy::loopback_only();
        assert_eq!(
            policy
                .validate("http://127.0.0.1:8000/v1")
                .unwrap()
                .classification,
            EndpointClassification::Loopback
        );
        assert_eq!(
            policy
                .validate("http://[::1]:8000/v1")
                .unwrap()
                .classification,
            EndpointClassification::Loopback
        );
    }

    #[test]
    fn accepts_only_operator_approved_cidrs() {
        let policy =
            LocalEndpointPolicy::new(["10.20.0.0/16", "fd12::/16"], std::iter::empty::<&str>())
                .unwrap();
        assert!(policy.validate("http://10.20.4.2:8000").is_ok());
        assert!(policy.validate("http://[fd12::5]:8000").is_ok());
        assert!(matches!(
            policy.validate("http://8.8.8.8"),
            Err(PolicyError::UnapprovedAddress(_))
        ));
    }

    #[test]
    fn rejects_unapproved_dns_and_mixed_answers() {
        let policy = LocalEndpointPolicy::new(["10.0.0.0/8"], ["model.lan"]).unwrap();
        assert!(matches!(
            policy.validate_with(
                "http://public.example",
                &FakeResolver::new("public.example", &["10.1.2.3:80"])
            ),
            Err(PolicyError::UnapprovedDns(_))
        ));
        let mixed = FakeResolver::new("model.lan", &["10.1.2.3:80", "8.8.8.8:80"]);
        assert!(matches!(
            policy.validate_with("http://model.lan", &mixed),
            Err(PolicyError::UnapprovedAddress(_))
        ));
    }

    #[test]
    fn rejects_userinfo_and_alternate_numeric_forms() {
        let policy = LocalEndpointPolicy::loopback_only();
        assert!(matches!(
            policy.validate("http://user@127.0.0.1"),
            Err(PolicyError::Userinfo)
        ));
        for url in [
            "http://2130706433",
            "http://0x7f000001",
            "http://0177.0.0.1",
        ] {
            assert!(
                matches!(policy.validate(url), Err(PolicyError::AlternateNumericHost)),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn resolver_is_used_once_and_client_is_pinned_for_rebinding_safety() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let policy = LocalEndpointPolicy::new(["10.0.0.0/8"], ["model.lan"]).unwrap();
        let resolver = FakeResolver::new("model.lan", &[&format!("127.0.0.1:{port}")]);
        let resolution = policy
            .validate_with(&format!("http://model.lan:{port}"), &resolver)
            .unwrap();
        resolver
            .answers
            .lock()
            .unwrap()
            .insert("model.lan".into(), vec!["8.8.8.8:80".parse().unwrap()]);
        let client = policy
            .reqwest_client(&resolution, |builder| builder)
            .unwrap();
        let response = client.get(resolution.url.clone()).send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn redirects_are_disabled_in_policy_clients() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://8.8.8.8/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let policy = LocalEndpointPolicy::loopback_only();
        let resolution = policy
            .validate(&format!("http://127.0.0.1:{port}"))
            .unwrap();
        let client = policy
            .reqwest_client(&resolution, |builder| builder)
            .unwrap();
        let response = client.get(resolution.url.clone()).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.join().unwrap();
    }
}
