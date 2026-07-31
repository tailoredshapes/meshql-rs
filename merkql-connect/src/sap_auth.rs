//! How merkql-connect authenticates to a SAP system, shared by every SAP source.
//!
//! # Why this is one module and not one copy per connector
//!
//! [`crate::sap`] carries a note explaining that its OData walk and its auth
//! were duplicated from `tailoredshapes/sap-cdc-mcp` rather than shared, and the
//! reasons it gives are all about that being **a different git repository**: a
//! shared crate there means a GitHub-git-dep pinned to a tag and a two-repo
//! release for every fix.
//!
//! None of that argument survives inside one crate. `sap_odp` needs the same six
//! modes against the same gateways, and a second copy of credential handling in
//! the same `src/` directory is a copy that will be fixed once. The failure that
//! prevents is specific: the OAuth cache below refreshes a minute *before*
//! expiry (see [`AuthedClient::oauth_token`]) because a token that dies between
//! the check and the request comes back as a 401, and a 401 mid-cycle is a fatal
//! backend error that kills the connector. A duplicate that drifted on that
//! margin would be a connector that falls over on a timer, in one source only,
//! for reasons the other source's tests already cover.
//!
//! So: **one implementation, two sources.** The module is compiled only when a
//! SAP source is, and nothing here is `pub` — a type holding live credentials
//! that code outside this crate can pattern-match is a credential one `{:?}`
//! away from a log file.
//!
//! # Six modes, because SAP deployments genuinely have six shapes
//!
//! Basic (on-prem gateway), OAuth2 client-credentials (BTP destination service),
//! OAuth2 SAML-bearer (principal propagation), X.509 mTLS (BTP mTLS
//! destinations), Bearer (an SLT or API-management proxy that has already done
//! the exchange) and None (mTLS terminated ahead of the connector, or a private
//! network — and the mode the tests use when auth is not what is under test).

use crate::config::SapAuthConfig;
use crate::source::CdcError;
use reqwest::{Client, ClientBuilder, RequestBuilder};
use serde_json::Value as Json;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Secrets
// ─────────────────────────────────────────────────────────────────────────────

/// A string that does not print itself.
///
/// Credentials reach this module from the environment and then sit inside a
/// long-lived struct that participates in `{:?}` error context. `SecretString`
/// from the `secrecy` crate does the same job; a ten-line newtype does it
/// without a dependency the workspace does not otherwise carry.
#[derive(Clone)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

pub(crate) fn env_var(name: &str) -> Result<String, CdcError> {
    std::env::var(name).map_err(|_| {
        CdcError::Backend(anyhow::anyhow!(
            "SAP auth needs environment variable {name}, which is unset. Credentials are \
             deliberately not readable from the connector TOML — the config names the \
             variable, the deployment supplies the value."
        ))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth
// ─────────────────────────────────────────────────────────────────────────────

/// A resolved SAP auth strategy, with the secret material already pulled out of
/// the environment.
#[derive(Debug, Clone)]
pub(crate) enum SapAuth {
    /// No credentials. Only correct when something else — mTLS at a proxy, a
    /// private network — is doing the authenticating.
    None,
    Basic {
        user: String,
        pass: Secret,
    },
    Bearer {
        token: Secret,
    },
    Oauth2ClientCredentials {
        token_url: String,
        client_id: String,
        client_secret: Secret,
        scope: Option<String>,
    },
    Oauth2SamlBearer {
        token_url: String,
        assertion: Secret,
        client_id: String,
    },
    /// A PEM cert + key pair on disk. Not an env var, because a certificate is
    /// a file and stuffing PEM into the environment is how a private key ends
    /// up in a process listing.
    MTls {
        cert_pem: Vec<u8>,
    },
}

impl SapAuth {
    /// Resolve a config-declared mode into live credentials.
    pub(crate) fn resolve(config: &SapAuthConfig) -> Result<Self, CdcError> {
        Ok(match config {
            SapAuthConfig::None => SapAuth::None,
            SapAuthConfig::Basic { user_env, pass_env } => SapAuth::Basic {
                user: env_var(user_env)?,
                pass: Secret::new(env_var(pass_env)?),
            },
            SapAuthConfig::Bearer { token_env } => SapAuth::Bearer {
                token: Secret::new(env_var(token_env)?),
            },
            SapAuthConfig::Oauth2Cc {
                token_url,
                client_id_env,
                client_secret_env,
                scope,
            } => SapAuth::Oauth2ClientCredentials {
                token_url: token_url.clone(),
                client_id: env_var(client_id_env)?,
                client_secret: Secret::new(env_var(client_secret_env)?),
                scope: scope.clone(),
            },
            SapAuthConfig::Oauth2SamlBearer {
                token_url,
                assertion_env,
                client_id_env,
            } => SapAuth::Oauth2SamlBearer {
                token_url: token_url.clone(),
                assertion: Secret::new(env_var(assertion_env)?),
                client_id: env_var(client_id_env)?,
            },
            SapAuthConfig::Mtls {
                cert_path,
                key_path,
            } => SapAuth::MTls {
                cert_pem: read_identity_pem(cert_path, key_path)?,
            },
        })
    }
}

/// `reqwest::Identity::from_pem` wants the certificate and the key in one blob.
/// Read them at *startup* rather than at first request: a deployment with an
/// unreadable key should fail before it claims a merkql topic, not an hour
/// later on the first poll.
fn read_identity_pem(cert_path: &Path, key_path: &Path) -> Result<Vec<u8>, CdcError> {
    let mut pem = std::fs::read(cert_path).map_err(|e| {
        CdcError::Backend(anyhow::anyhow!(
            "reading mTLS certificate {}: {e}",
            cert_path.display()
        ))
    })?;
    let key = std::fs::read(key_path).map_err(|e| {
        CdcError::Backend(anyhow::anyhow!(
            "reading mTLS private key {}: {e}",
            key_path.display()
        ))
    })?;
    pem.push(b'\n');
    pem.extend_from_slice(&key);
    Ok(pem)
}

#[derive(Clone)]
struct CachedToken {
    bearer: Secret,
    expires_at: Instant,
}

/// A `reqwest::Client` that knows how to authenticate to SAP, and caches an
/// OAuth token until a minute before it expires.
pub(crate) struct AuthedClient {
    pub(crate) client: Client,
    auth: SapAuth,
    token: Mutex<Option<CachedToken>>,
}

impl AuthedClient {
    pub(crate) fn new(auth: SapAuth) -> Result<Self, CdcError> {
        let mut builder = ClientBuilder::new()
            .user_agent(concat!("merkql-connect/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120));

        if let SapAuth::MTls { cert_pem } = &auth {
            let identity = reqwest::Identity::from_pem(cert_pem).map_err(|e| {
                CdcError::Backend(anyhow::anyhow!(
                    "the configured mTLS certificate and key are not a usable identity: {e}"
                ))
            })?;
            builder = builder.identity(identity);
        }

        let client = builder
            .build()
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("building the SAP HTTP client: {e}")))?;

        Ok(Self {
            client,
            auth,
            token: Mutex::new(None),
        })
    }

    /// Attach whatever this strategy needs to a request.
    pub(crate) async fn authorize(
        &self,
        request: RequestBuilder,
    ) -> Result<RequestBuilder, CdcError> {
        use base64::Engine;
        Ok(match &self.auth {
            // mTLS authenticates at the TLS handshake; there is no header.
            SapAuth::None | SapAuth::MTls { .. } => request,
            SapAuth::Basic { user, pass } => {
                let creds = base64::engine::general_purpose::STANDARD
                    .encode(format!("{user}:{}", pass.expose()));
                request.header(reqwest::header::AUTHORIZATION, format!("Basic {creds}"))
            }
            SapAuth::Bearer { token } => request.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", token.expose()),
            ),
            SapAuth::Oauth2ClientCredentials { .. } | SapAuth::Oauth2SamlBearer { .. } => {
                let bearer = self.oauth_token().await?;
                request.header(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", bearer.expose()),
                )
            }
        })
    }

    async fn oauth_token(&self) -> Result<Secret, CdcError> {
        // Refresh a minute early. A token that expires between the check and
        // the request comes back as a 401 that this module would classify as a
        // backend error, so the margin is what keeps a long-running connector
        // from a periodic self-inflicted outage.
        //
        // The guard is dropped before the await deliberately: holding a
        // `std::sync::Mutex` across an await point is how a runtime deadlocks.
        {
            let cached = self.token.lock().expect("token cache poisoned").clone();
            if let Some(cached) = cached {
                if cached.expires_at > Instant::now() + Duration::from_secs(60) {
                    return Ok(cached.bearer);
                }
            }
        }

        let (token_url, form): (&str, Vec<(&str, String)>) = match &self.auth {
            SapAuth::Oauth2ClientCredentials {
                token_url,
                client_id,
                client_secret,
                scope,
            } => {
                let mut form = vec![
                    ("grant_type", "client_credentials".to_string()),
                    ("client_id", client_id.clone()),
                    ("client_secret", client_secret.expose().to_string()),
                ];
                if let Some(scope) = scope.as_deref().filter(|s| !s.is_empty()) {
                    form.push(("scope", scope.to_string()));
                }
                (token_url, form)
            }
            SapAuth::Oauth2SamlBearer {
                token_url,
                assertion,
                client_id,
            } => (
                token_url,
                vec![
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:saml2-bearer".to_string(),
                    ),
                    ("assertion", assertion.expose().to_string()),
                    ("client_id", client_id.clone()),
                ],
            ),
            _ => {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "oauth_token called for a non-OAuth strategy"
                )))
            }
        };

        let response = self
            .client
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("requesting an OAuth token: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            // Never include the body: an OAuth error response can echo the
            // assertion back.
            return Err(CdcError::Backend(anyhow::anyhow!(
                "the OAuth token endpoint {token_url} answered {status}"
            )));
        }

        let body: Json = response
            .json()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("parsing the OAuth token: {e}")))?;
        let access = body
            .get("access_token")
            .and_then(Json::as_str)
            .ok_or_else(|| {
                CdcError::Backend(anyhow::anyhow!(
                    "the OAuth token endpoint returned no access_token"
                ))
            })?;
        let ttl = body
            .get("expires_in")
            .and_then(Json::as_u64)
            .unwrap_or(3_600);

        let bearer = Secret::new(access);
        *self.token.lock().expect("token cache poisoned") = Some(CachedToken {
            bearer: bearer.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(bearer)
    }
}
