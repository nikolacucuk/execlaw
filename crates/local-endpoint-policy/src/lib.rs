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
    #[error("address {0} is not globally routable for public web access")]
    ProhibitedPublicAddress(IpAddr),
    #[error("DNS resolution for {0:?} returned no addresses")]
    EmptyResolution(String),
    #[error("DNS resolution failed for {host:?}: {message}")]
    Resolution { host: String, message: String },
    #[error("invalid approved CIDR {0:?}")]
    InvalidCidr(String),
    #[error("HTTP client build failed: {0}")]
    Client(String),
}

/// DNS-pinned target approved for public web fetching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicEgressResolution {
    pub url: Url,
    pub addresses: Vec<IpAddr>,
}

/// Resolve public destinations and pin HTTP connections to the checked addresses.
///
/// Local services use [`LocalEndpointPolicy`] instead; public web fetching
/// rejects every non-global or special-purpose address and validates each
/// redirect as a new destination.
#[derive(Debug, Clone, Copy, Default)]
pub struct PublicEgressPolicy;

impl PublicEgressPolicy {
    /// Resolve and validate a public HTTP(S) URL using the system resolver.
    pub fn resolve(&self, raw_url: &str) -> Result<PublicEgressResolution, PolicyError> {
        self.resolve_with(raw_url, &SystemResolver)
    }

    /// Resolve with an injected resolver for deterministic policy adapters.
    pub fn resolve_with(
        &self,
        raw_url: &str,
        resolver: &dyn Resolver,
    ) -> Result<PublicEgressResolution, PolicyError> {
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
        let addresses = match host {
            Host::Ipv4(address) => vec![IpAddr::V4(address)],
            Host::Ipv6(address) => vec![normalize_mapped_ip(IpAddr::V6(address))],
            Host::Domain(name) => resolver
                .resolve(name, port)
                .map_err(|error| PolicyError::Resolution {
                    host: name.to_owned(),
                    message: error.to_string(),
                })?
                .into_iter()
                .map(|socket| normalize_mapped_ip(socket.ip()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        };
        if addresses.is_empty() {
            return Err(PolicyError::EmptyResolution(
                url.host_str().unwrap_or_default().to_owned(),
            ));
        }
        for address in &addresses {
            if !is_globally_routable(*address) {
                return Err(PolicyError::ProhibitedPublicAddress(*address));
            }
        }
        Ok(PublicEgressResolution { url, addresses })
    }

    /// Build a direct, no-proxy client pinned to the checked addresses.
    ///
    /// Environment proxies are disabled because a proxy would resolve the
    /// hostname on the far side and bypass the address check performed here.
    pub fn reqwest_client(
        &self,
        resolution: &PublicEgressResolution,
        configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    ) -> Result<reqwest::Client, PolicyError> {
        if !matches!(resolution.url.scheme(), "http" | "https") {
            return Err(PolicyError::Scheme);
        }
        if !resolution.url.username().is_empty() || resolution.url.password().is_some() {
            return Err(PolicyError::Userinfo);
        }
        if resolution.addresses.is_empty() {
            return Err(PolicyError::EmptyResolution(
                resolution.url.host_str().unwrap_or_default().to_owned(),
            ));
        }
        for address in &resolution.addresses {
            if !is_globally_routable(*address) {
                return Err(PolicyError::ProhibitedPublicAddress(*address));
            }
        }
        // Apply caller options first so they cannot restore proxy resolution
        // or automatic redirects after this destination has been checked.
        let mut builder = configure(reqwest::Client::builder());
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
        builder
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| PolicyError::Client(error.to_string()))
    }
}

fn is_globally_routable(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        other => other,
    };
    match address {
        IpAddr::V4(v4) => {
            let prohibited = [
                "0.0.0.0/8",
                "10.0.0.0/8",
                "100.64.0.0/10",
                "127.0.0.0/8",
                "169.254.0.0/16",
                "172.16.0.0/12",
                "192.0.0.0/24",
                "192.0.2.0/24",
                "192.88.99.0/24",
                "192.168.0.0/16",
                "198.18.0.0/15",
                "198.51.100.0/24",
                "203.0.113.0/24",
                "224.0.0.0/4",
                "240.0.0.0/4",
            ];
            !prohibited.iter().any(|cidr| {
                cidr.parse::<IpNet>()
                    .is_ok_and(|network| network.contains(&IpAddr::V4(v4)))
            })
        }
        IpAddr::V6(v6) => {
            let global_unicast = "2000::/3"
                .parse::<IpNet>()
                .is_ok_and(|network| network.contains(&IpAddr::V6(v6)));
            let prohibited = [
                "2001::/23",
                "2001:db8::/32",
                "2002::/16",
                "3fff::/20",
                "64:ff9b::/96",
                "64:ff9b:1::/48",
            ];
            global_unicast
                && !prohibited.iter().any(|cidr| {
                    cidr.parse::<IpNet>()
                        .is_ok_and(|network| network.contains(&IpAddr::V6(v6)))
                })
        }
    }
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
        let addresses = addresses
            .into_iter()
            .map(normalize_mapped_ip)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
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
        // The final network policy must win over caller builder options.
        let mut builder = configure(reqwest::Client::builder());
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
        builder
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| PolicyError::Client(error.to_string()))
    }
}

/// Normalize IPv4-mapped IPv6 addresses before policy checks and pinning.
pub fn normalize_mapped_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(value) => value
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(value)),
        IpAddr::V4(_) => address,
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
    fn public_egress_rejects_private_answers_in_a_public_dns_set() {
        let policy = PublicEgressPolicy;
        let safe = FakeResolver::new("public.example", &["93.184.216.34:443"]);
        assert!(
            policy
                .resolve_with("https://public.example/path", &safe)
                .is_ok()
        );

        let rebinding = FakeResolver::new(
            "public.example",
            &["93.184.216.34:443", "169.254.169.254:443"],
        );
        assert!(matches!(
            policy.resolve_with("https://public.example/path", &rebinding),
            Err(PolicyError::ProhibitedPublicAddress(_))
        ));
    }

    #[test]
    fn public_endpoint_adversarial_address_matrix() {
        let policy = PublicEgressPolicy;
        for answer in [
            "127.0.0.1:443",
            "10.1.2.3:443",
            "169.254.169.254:443",
            "192.168.1.2:443",
            "[::1]:443",
            "[fc00::1]:443",
            "[::ffff:127.0.0.1]:443",
        ] {
            let resolver = FakeResolver::new("public.example", &["93.184.216.34:443", answer]);
            assert!(
                matches!(
                    policy.resolve_with("https://public.example/data", &resolver),
                    Err(PolicyError::ProhibitedPublicAddress(_))
                ),
                "mixed DNS answer {answer} must deny the whole destination"
            );
        }
        for url in [
            "http://127.0.0.1/private",
            "http://[::ffff:127.0.0.1]/private",
            "http://2130706433/private",
            "http://0x7f000001/private",
            "http://0177.0.0.1/private",
        ] {
            assert!(policy.resolve(url).is_err(), "public URL {url} must fail");
        }
    }

    #[test]
    fn private_endpoint_adversarial_address_matrix() {
        let policy = LocalEndpointPolicy::new(["10.20.0.0/16"], ["approved.lan"]).unwrap();
        for answer in ["8.8.8.8:80", "10.21.0.1:80", "[::ffff:8.8.8.8]:80"] {
            let resolver = FakeResolver::new("approved.lan", &["10.20.1.2:80", answer]);
            assert!(
                matches!(
                    policy.validate_with("http://approved.lan/resource", &resolver),
                    Err(PolicyError::UnapprovedAddress(_))
                ),
                "unapproved DNS answer {answer} must deny the whole destination"
            );
        }
        assert!(policy.validate("http://10.20.1.2/resource").is_ok());
        assert!(matches!(
            policy.validate_with(
                "http://unapproved.lan/resource",
                &FakeResolver::new("unapproved.lan", &["10.20.1.2:80"])
            ),
            Err(PolicyError::UnapprovedDns(_))
        ));
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
    fn ipv4_mapped_ipv6_addresses_share_ipv4_policy_and_pins() {
        let loopback = LocalEndpointPolicy::loopback_only()
            .validate("http://[::ffff:127.0.0.1]:8000/v1")
            .unwrap();
        assert_eq!(loopback.classification, EndpointClassification::Loopback);
        assert_eq!(
            loopback.addresses,
            vec!["127.0.0.1".parse::<IpAddr>().unwrap()]
        );

        let approved = LocalEndpointPolicy::new(["10.0.0.0/8"], std::iter::empty::<&str>())
            .unwrap()
            .validate("http://[::ffff:10.1.2.3]:8000")
            .unwrap();
        assert_eq!(
            approved.addresses,
            vec!["10.1.2.3".parse::<IpAddr>().unwrap()]
        );
        assert!(matches!(
            LocalEndpointPolicy::new(["10.0.0.0/8"], std::iter::empty::<&str>())
                .unwrap()
                .validate("http://[::ffff:172.16.0.1]:8000"),
            Err(PolicyError::UnapprovedAddress(IpAddr::V4(_)))
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

    #[tokio::test]
    async fn caller_configuration_cannot_restore_proxy_or_redirect() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let count = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..count])
                    .to_ascii_lowercase()
                    .contains("host: approved.lan")
            );
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let policy =
            LocalEndpointPolicy::new(std::iter::empty::<&str>(), ["approved.lan"]).unwrap();
        let resolver = FakeResolver::new("approved.lan", &[&format!("127.0.0.1:{port}")]);
        let resolution = policy
            .validate_with(&format!("http://approved.lan:{port}/"), &resolver)
            .unwrap();
        let client = policy
            .reqwest_client(&resolution, |builder| {
                builder
                    .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .timeout(std::time::Duration::from_secs(2))
            })
            .unwrap();
        let response = client.get(resolution.url).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.join().unwrap();
    }
}
