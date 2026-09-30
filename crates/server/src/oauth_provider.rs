//! OAuth 2.0 provider primitives — the pure, network-talking pieces
//! the admin endpoints + refresh sweeper sit on top of.
//!
//! Today there's exactly one provider (Google) wired up. The trait
//! lives behind `OauthProvider` so the second provider lands as a
//! sibling impl without rewriting the `oauth_admin` endpoints or
//! the refresh sweeper. The provider trait is intentionally small:
//! build authorize URL, exchange code, refresh access token, fetch
//! userinfo. Everything else (state-token CSRF, persistence,
//! redirect bookkeeping) lives in the caller because it's the same
//! across providers.
//!
//! Network calls go through a `reqwest::Client` the caller injects
//! so tests can hand in a mock that points at `httpmock` /
//! `wiremock`. For prod, `GoogleOauthProvider::default_client()`
//! returns a sane reqwest with rustls + reasonable timeouts.

use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;
use url::Url;

#[derive(Error)]
pub enum OauthProviderError {
    #[error("OAuth provider HTTP request failed")]
    Http(String),
    #[error("OAuth provider returned HTTP {status} (response body redacted)")]
    Status { status: u16, body: String },
    #[error("OAuth provider response could not be decoded")]
    Decode(String),
    #[error("missing field in response: {0}")]
    MissingField(&'static str),
}

impl std::fmt::Debug for OauthProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(_) => f.write_str("Http(<redacted>)"),
            Self::Status { status, .. } => f
                .debug_struct("Status")
                .field("status", status)
                .field("body", &"<redacted>")
                .finish(),
            Self::Decode(_) => f.write_str("Decode(<redacted>)"),
            Self::MissingField(field) => f.debug_tuple("MissingField").field(field).finish(),
        }
    }
}

#[derive(Clone)]
pub struct AuthorizeParams {
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    /// Random per-request CSRF token; the caller has already
    /// persisted this in `state_oauth_pending`.
    pub state_token: String,
}

impl std::fmt::Debug for AuthorizeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizeParams")
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("scopes", &self.scopes)
            .field("state_token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct ExchangeParams {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub code: String,
}

impl std::fmt::Debug for ExchangeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangeParams")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .field("code", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct RefreshParams {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

impl std::fmt::Debug for RefreshParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshParams")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

/// Successful token-grant response. Both code-exchange and
/// refresh-grant land here.
#[derive(Clone)]
pub struct TokenGrant {
    pub access_token: String,
    /// Some providers (Google after the first consent) omit a fresh
    /// refresh_token on the refresh-grant path. Caller handles the
    /// preserve-on-NULL semantics in `OauthTokenStore::upsert`.
    pub refresh_token: Option<String>,
    pub expires_in_secs: i64,
    pub scope: Option<String>,
    pub id_token: Option<String>,
}

impl std::fmt::Debug for TokenGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenGrant")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in_secs", &self.expires_in_secs)
            .field("scope", &self.scope)
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Userinfo response — for the email field we surface in the
/// "Connected as user@example.com" badge.
#[derive(Debug, Clone)]
pub struct Userinfo {
    pub email: Option<String>,
    pub name: Option<String>,
}

#[async_trait]
pub trait OauthProvider: Send + Sync {
    fn provider_id(&self) -> &'static str;

    /// Construct the URL the operator's browser opens to consent.
    /// Pure (no I/O) — string concatenation against the provider's
    /// authorize endpoint.
    fn build_authorize_url(&self, params: &AuthorizeParams) -> Result<String, OauthProviderError>;

    /// Exchange the `code` from the callback for an access /
    /// refresh token pair.
    async fn exchange_code(
        &self,
        params: &ExchangeParams,
    ) -> Result<TokenGrant, OauthProviderError>;

    /// Use a refresh_token to mint a fresh access_token. Most
    /// providers don't return a new refresh_token here.
    async fn refresh_access_token(
        &self,
        params: &RefreshParams,
    ) -> Result<TokenGrant, OauthProviderError>;

    /// Look up the email of the account these tokens belong to.
    async fn fetch_userinfo(&self, access_token: &str) -> Result<Userinfo, OauthProviderError>;
}

// ---------------------------------------------------------------------------
// Google.

const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";

#[derive(Debug, Clone)]
pub struct GoogleOauthProvider {
    client: reqwest::Client,
    client_error: Option<String>,
    enforce_public_policy: bool,
    /// Override the authorize URL (tests). None = production URL.
    authorize_url: String,
    token_url: String,
    userinfo_url: String,
}

impl Default for GoogleOauthProvider {
    fn default() -> Self {
        match Self::default_client() {
            Ok(client) => Self {
                enforce_public_policy: true,
                ..Self::new(client)
            },
            Err(error) => Self {
                // Keep URL construction available, but network methods fail closed.
                client: reqwest::Client::new(),
                client_error: Some(error),
                enforce_public_policy: true,
                authorize_url: GOOGLE_AUTHORIZE_URL.into(),
                token_url: GOOGLE_TOKEN_URL.into(),
                userinfo_url: GOOGLE_USERINFO_URL.into(),
            },
        }
    }
}

impl GoogleOauthProvider {
    fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            client_error: None,
            enforce_public_policy: false,
            authorize_url: GOOGLE_AUTHORIZE_URL.into(),
            token_url: GOOGLE_TOKEN_URL.into(),
            userinfo_url: GOOGLE_USERINFO_URL.into(),
        }
    }

    /// Test-only constructor that points the token + userinfo URLs
    /// at a local `httpmock` / `wiremock` server.
    #[cfg(test)]
    pub fn with_endpoints(
        client: reqwest::Client,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
        userinfo_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            client_error: None,
            enforce_public_policy: false,
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            userinfo_url: userinfo_url.into(),
        }
    }

    /// Build a direct client for fixed Google endpoints with redirects disabled.
    pub fn default_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("execlaw/0.1 oauth-client")
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("build direct OAuth client: {error}"))
    }

    fn ensure_client_ready(&self) -> Result<(), OauthProviderError> {
        match &self.client_error {
            Some(error) => Err(OauthProviderError::Http(error.clone())),
            None => Ok(()),
        }
    }

    fn client_for(&self, url: &str) -> Result<reqwest::Client, OauthProviderError> {
        self.ensure_client_ready()?;
        if !self.enforce_public_policy {
            return Ok(self.client.clone());
        }
        crate::local_endpoint_policy::checked_public_client(url, |builder| {
            builder
                .timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(5))
                .user_agent("execlaw/0.1 oauth-client")
        })
        .map_err(OauthProviderError::Http)
    }
}

#[async_trait]
impl OauthProvider for GoogleOauthProvider {
    fn provider_id(&self) -> &'static str {
        "google"
    }

    fn build_authorize_url(&self, params: &AuthorizeParams) -> Result<String, OauthProviderError> {
        // access_type=offline + prompt=consent so we ALWAYS get a
        // refresh_token. Without prompt=consent, Google omits the
        // refresh_token on a re-consent (returning user) and the
        // operator silently loses the long-lived secret.
        let mut url = Url::parse(&self.authorize_url)
            .map_err(|e| OauthProviderError::Http(format!("authorize url: {e}")))?;
        let scope = params.scopes.join(" ");
        url.query_pairs_mut()
            .append_pair("client_id", &params.client_id)
            .append_pair("redirect_uri", &params.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &scope)
            .append_pair("state", &params.state_token)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("include_granted_scopes", "true");
        Ok(url.to_string())
    }

    async fn exchange_code(
        &self,
        params: &ExchangeParams,
    ) -> Result<TokenGrant, OauthProviderError> {
        let client = self.client_for(&self.token_url)?;
        let form = [
            ("grant_type", "authorization_code"),
            ("code", params.code.as_str()),
            ("client_id", params.client_id.as_str()),
            ("client_secret", params.client_secret.as_str()),
            ("redirect_uri", params.redirect_uri.as_str()),
        ];
        post_token_grant(&client, &self.token_url, &form).await
    }

    async fn refresh_access_token(
        &self,
        params: &RefreshParams,
    ) -> Result<TokenGrant, OauthProviderError> {
        let client = self.client_for(&self.token_url)?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", params.refresh_token.as_str()),
            ("client_id", params.client_id.as_str()),
            ("client_secret", params.client_secret.as_str()),
        ];
        post_token_grant(&client, &self.token_url, &form).await
    }

    async fn fetch_userinfo(&self, access_token: &str) -> Result<Userinfo, OauthProviderError> {
        let client = self.client_for(&self.userinfo_url)?;
        let resp = client
            .get(&self.userinfo_url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| OauthProviderError::Http(e.to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| OauthProviderError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(OauthProviderError::Status {
                status: status.as_u16(),
                body,
            });
        }
        #[derive(Deserialize)]
        struct R {
            email: Option<String>,
            name: Option<String>,
        }
        let r: R =
            serde_json::from_str(&body).map_err(|e| OauthProviderError::Decode(e.to_string()))?;
        Ok(Userinfo {
            email: r.email,
            name: r.name,
        })
    }
}

async fn post_token_grant(
    client: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<TokenGrant, OauthProviderError> {
    let resp = client
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| OauthProviderError::Http(e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| OauthProviderError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(OauthProviderError::Status {
            status: status.as_u16(),
            body,
        });
    }
    #[derive(Deserialize)]
    struct R {
        access_token: Option<String>,
        refresh_token: Option<String>,
        expires_in: Option<i64>,
        scope: Option<String>,
        id_token: Option<String>,
    }
    let r: R =
        serde_json::from_str(&body).map_err(|e| OauthProviderError::Decode(e.to_string()))?;
    let access_token = r
        .access_token
        .ok_or(OauthProviderError::MissingField("access_token"))?;
    let expires_in = r
        .expires_in
        .ok_or(OauthProviderError::MissingField("expires_in"))?;
    Ok(TokenGrant {
        access_token,
        refresh_token: r.refresh_token,
        expires_in_secs: expires_in,
        scope: r.scope,
        id_token: r.id_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_oauth_denies_private_destination_before_a_request() {
        let mut provider = GoogleOauthProvider::default();
        provider.token_url = "http://169.254.169.254/token".into();
        assert!(provider.client_for(&provider.token_url).is_err());
        provider.userinfo_url = "http://127.0.0.1/userinfo".into();
        assert!(provider.client_for(&provider.userinfo_url).is_err());
    }

    #[test]
    fn oauth_provider_debug_output_redacts_credentials_and_grants() {
        let exchange = ExchangeParams {
            client_id: "public-client".into(),
            client_secret: "incident-client-secret-marker".into(),
            redirect_uri: "http://localhost/callback".into(),
            code: "incident-authorization-code-marker".into(),
        };
        let exchange_debug = format!("{exchange:?}");
        assert!(exchange_debug.contains("<redacted>"));
        assert!(!exchange_debug.contains("incident-client-secret-marker"));
        assert!(!exchange_debug.contains("incident-authorization-code-marker"));

        let grant = TokenGrant {
            access_token: "incident-access-token-marker".into(),
            refresh_token: Some("incident-refresh-token-marker".into()),
            expires_in_secs: 3600,
            scope: None,
            id_token: Some("incident-id-token-marker".into()),
        };
        let grant_debug = format!("{grant:?}");
        assert!(grant_debug.contains("<redacted>"));
        assert!(!grant_debug.contains("incident-access-token-marker"));
        assert!(!grant_debug.contains("incident-refresh-token-marker"));
        assert!(!grant_debug.contains("incident-id-token-marker"));

        let provider_error = OauthProviderError::Status {
            status: 500,
            body: "incident-provider-secret-marker".into(),
        };
        let error_debug = format!("{provider_error:?}");
        let error_display = provider_error.to_string();
        assert!(error_debug.contains("<redacted>"));
        assert!(error_display.contains("redacted"));
        assert!(!error_debug.contains("incident-provider-secret-marker"));
        assert!(!error_display.contains("incident-provider-secret-marker"));
    }

    #[tokio::test]
    async fn oauth_token_redirect_does_not_forward_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let token_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_address = token_listener.local_addr().unwrap();
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let token_server = tokio::spawn(async move {
            let (mut socket, _) = token_listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/capture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let provider = GoogleOauthProvider::with_endpoints(
            GoogleOauthProvider::default_client().unwrap(),
            format!("http://{token_address}/authorize"),
            format!("http://{token_address}/token"),
            format!("http://{token_address}/userinfo"),
        );
        let result = provider
            .exchange_code(&ExchangeParams {
                client_id: "client-id".into(),
                client_secret: "secret-must-stay-at-token-endpoint".into(),
                redirect_uri: "http://127.0.0.1/callback".into(),
                code: "authorization-code".into(),
            })
            .await;
        assert!(matches!(
            result,
            Err(OauthProviderError::Status { status: 302, .. })
        ));
        token_server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), target_listener.accept())
                .await
                .is_err(),
            "redirect destination must receive no request"
        );
    }

    #[test]
    fn build_authorize_url_includes_required_params() {
        let p = GoogleOauthProvider::default();
        let url = p
            .build_authorize_url(&AuthorizeParams {
                client_id: "abc.apps.googleusercontent.com".into(),
                redirect_uri: "http://localhost:3031/api/oauth/google/callback".into(),
                scopes: vec![
                    "https://www.googleapis.com/auth/contacts.readonly".to_owned(),
                    "openid".to_owned(),
                    "email".to_owned(),
                ],
                state_token: "csrf-xyz".into(),
            })
            .unwrap();
        // Required pieces.
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=abc.apps.googleusercontent.com"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=csrf-xyz"));
        // access_type=offline + prompt=consent — without these
        // Google won't reliably return a refresh_token.
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        // Scope joined with space (URL-encoded as +).
        assert!(url.contains("scope=https"));
        assert!(url.contains("contacts.readonly"));
    }

    #[tokio::test]
    async fn exchange_code_decodes_token_grant() {
        // Local one-shot HTTP server simulating Google's token endpoint.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = serde_json::json!({
                "access_token": "ya29.access",
                "refresh_token": "1//refresh",
                "expires_in": 3599,
                "scope": "https://www.googleapis.com/auth/contacts.readonly",
                "token_type": "Bearer",
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let p = GoogleOauthProvider::with_endpoints(
            reqwest::Client::new(),
            GOOGLE_AUTHORIZE_URL,
            format!("http://{addr}/token"),
            GOOGLE_USERINFO_URL,
        );
        let g = p
            .exchange_code(&ExchangeParams {
                client_id: "cid".into(),
                client_secret: "secret".into(),
                redirect_uri: "http://localhost:3031/cb".into(),
                code: "auth-code".into(),
            })
            .await
            .unwrap();
        assert_eq!(g.access_token, "ya29.access");
        assert_eq!(g.refresh_token.as_deref(), Some("1//refresh"));
        assert_eq!(g.expires_in_secs, 3599);
    }

    #[tokio::test]
    async fn token_endpoint_error_surfaces_status_and_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = r#"{"error":"invalid_grant","error_description":"Bad code"}"#;
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let p = GoogleOauthProvider::with_endpoints(
            reqwest::Client::new(),
            GOOGLE_AUTHORIZE_URL,
            format!("http://{addr}/token"),
            GOOGLE_USERINFO_URL,
        );
        let err = p
            .exchange_code(&ExchangeParams {
                client_id: "cid".into(),
                client_secret: "secret".into(),
                redirect_uri: "http://localhost:3031/cb".into(),
                code: "stale".into(),
            })
            .await
            .unwrap_err();
        match err {
            OauthProviderError::Status { status, body } => {
                assert_eq!(status, 400);
                assert!(body.contains("invalid_grant"));
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_userinfo_returns_email_when_present() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = r#"{"email":"alice@example.com","name":"Alice"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let p = GoogleOauthProvider::with_endpoints(
            reqwest::Client::new(),
            GOOGLE_AUTHORIZE_URL,
            GOOGLE_TOKEN_URL,
            format!("http://{addr}/userinfo"),
        );
        let u = p.fetch_userinfo("ya29.access").await.unwrap();
        assert_eq!(u.email.as_deref(), Some("alice@example.com"));
        assert_eq!(u.name.as_deref(), Some("Alice"));
    }
}
