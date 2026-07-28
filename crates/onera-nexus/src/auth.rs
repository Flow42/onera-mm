//! Personal API-key authentication.
//!
//! This is one implementation of [`AuthProvider`]. The abstraction exists so a
//! registered-application flow (Nexus SSO, OAuth) can be dropped in later
//! without touching [`crate::client::NexusClient`] or anything in the core: the
//! client only ever asks for a [`Credential`].
//!
//! The key is written to the platform secret store and nowhere else. There is
//! deliberately no file fallback — if Secret Service is unavailable, storing
//! fails and the user is told, rather than a credential silently landing in a
//! config file.

use async_trait::async_trait;
use onera_core::ids::ProviderId;
use onera_core::ports::{AccountInfo, AuthProvider, Credential, SecretStore};
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};
use std::sync::Arc;

/// Key under which the Nexus personal API key is stored.
pub const SECRET_KEY: &str = "nexus.api_key";

/// The v1 endpoint that validates a key and returns the account.
const VALIDATE_PATH: &str = "/users/validate.json";

/// Authenticates with a user-supplied personal API key.
pub struct ApiKeyAuth {
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
    v1_base: String,
}

impl ApiKeyAuth {
    /// Build the provider.
    ///
    /// # Errors
    /// Fails if the HTTP stack cannot be initialized.
    pub fn new(
        secrets: Arc<dyn SecretStore>,
        v1_base: impl Into<String>,
        user_agent: &str,
    ) -> Result<Self> {
        Self::build(secrets, v1_base, user_agent, true)
    }

    /// Build a provider that will also talk to a plain-HTTP server.
    ///
    /// For tests against a local mock server only; [`ApiKeyAuth::new`] refuses
    /// anything but HTTPS, and that is what every shipped binary calls.
    ///
    /// # Errors
    /// Fails if the HTTP stack cannot be initialized.
    pub fn new_for_tests(
        secrets: Arc<dyn SecretStore>,
        v1_base: impl Into<String>,
        user_agent: &str,
    ) -> Result<Self> {
        Self::build(secrets, v1_base, user_agent, false)
    }

    fn build(
        secrets: Arc<dyn SecretStore>,
        v1_base: impl Into<String>,
        user_agent: &str,
        https_only: bool,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent.to_owned())
            .timeout(std::time::Duration::from_secs(20))
            .https_only(https_only)
            .build()
            .map_err(|e| CoreError::Provider(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            secrets,
            http,
            v1_base: v1_base.into(),
        })
    }

    /// Reject keys that cannot possibly be valid before spending a request.
    ///
    /// This is a shape check, not a security check — the server is the
    /// authority — but it turns the commonest mistake (pasting a URL, or an
    /// empty field) into an instant, clear message.
    fn precheck(key: &Secret) -> Result<()> {
        let raw = key.expose();
        if raw.trim().is_empty() {
            return Err(CoreError::InvalidInput("the API key is empty".into()));
        }
        if raw.trim().len() != raw.len() {
            return Err(CoreError::InvalidInput(
                "the API key has leading or trailing whitespace".into(),
            ));
        }
        if raw.len() < 20 {
            return Err(CoreError::InvalidInput(
                "that does not look like a Nexus API key; copy the whole key from https://www.nexusmods.com/users/myaccount?tab=api".into(),
            ));
        }
        if raw.contains("://") || raw.contains(' ') {
            return Err(CoreError::InvalidInput(
                "that looks like a URL rather than an API key".into(),
            ));
        }
        Ok(())
    }
}

/// The account payload the validate endpoint returns.
#[derive(Debug, serde::Deserialize)]
struct ValidateResponse {
    #[serde(alias = "user_id")]
    user_id: Option<serde_json::Value>,
    #[serde(alias = "name")]
    name: Option<String>,
    #[serde(alias = "is_premium", alias = "is_premium?")]
    is_premium: Option<bool>,
    email: Option<String>,
}

#[async_trait]
impl AuthProvider for ApiKeyAuth {
    fn provider_id(&self) -> ProviderId {
        ProviderId::nexus()
    }

    async fn is_authenticated(&self) -> Result<bool> {
        Ok(self.secrets.get(SECRET_KEY).await?.is_some())
    }

    async fn credential(&self) -> Result<Credential> {
        let key =
            self.secrets
                .get(SECRET_KEY)
                .await?
                .ok_or_else(|| CoreError::Unauthenticated {
                    provider: "nexus".to_owned(),
                })?;
        Ok(Credential::ApiKey(key))
    }

    async fn validate(&self, credential: &Credential) -> Result<AccountInfo> {
        let Credential::ApiKey(key) = credential else {
            return Err(CoreError::Unsupported(
                "this provider only accepts a personal API key".into(),
            ));
        };
        Self::precheck(key)?;

        let url = format!("{}{VALIDATE_PATH}", self.v1_base.trim_end_matches('/'));
        let response = self
            .http
            .get(&url)
            .header("apikey", key.expose())
            .send()
            .await
            .map_err(|e| {
                // The error text can echo the request; scrub it before it is
                // ever displayed or logged.
                CoreError::Provider(onera_core::redact::scrub(&e.to_string(), &[key]))
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(CoreError::Unauthenticated {
                provider: "nexus".to_owned(),
            });
        }
        if !status.is_success() {
            return Err(crate::error::map_status(
                status.as_u16(),
                crate::error::parse_problem(&body),
                None,
            ));
        }

        let parsed: ValidateResponse = serde_json::from_str(&body)
            .map_err(|e| CoreError::Provider(format!("unreadable validation response: {e}")))?;
        Ok(AccountInfo {
            provider_user_id: parsed
                .user_id
                .map(|v| v.to_string().trim_matches('"').to_owned())
                .unwrap_or_default(),
            username: parsed.name.unwrap_or_else(|| "Nexus user".to_owned()),
            premium: parsed.is_premium,
            email: parsed.email,
        })
    }

    async fn store(&self, credential: Credential) -> Result<AccountInfo> {
        // Validate first: an invalid key must never reach the secret store, so
        // "authenticated" and "has a stored key" cannot drift apart.
        let account = self.validate(&credential).await?;
        let Credential::ApiKey(key) = credential else {
            return Err(CoreError::Unsupported("expected a personal API key".into()));
        };

        if !self.secrets.is_available().await {
            return Err(CoreError::SecretStore(
                "the Linux Secret Service is not available; Onera will not store an API key in plain text"
                    .to_owned(),
            ));
        }
        self.secrets.set(SECRET_KEY, &key).await?;
        Ok(account)
    }

    async fn forget(&self) -> Result<()> {
        self.secrets.delete(SECRET_KEY).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obviously_wrong_keys_are_rejected_before_a_request_is_made() {
        let cases = [
            ("", "empty"),
            ("   ", "empty"),
            ("short", "does not look like"),
            ("https://www.nexusmods.com/users/myaccount?tab=api", "URL"),
            ("  a-key-that-is-long-enough-here  ", "whitespace"),
        ];
        for (raw, expected) in cases {
            let err = ApiKeyAuth::precheck(&Secret::new(raw)).unwrap_err();
            assert!(
                format!("{err}").contains(expected),
                "{raw:?} produced {err}, expected something about {expected:?}"
            );
        }
    }

    #[test]
    fn a_plausible_key_passes_the_shape_check() {
        let key = Secret::new("aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789--abc");
        assert!(ApiKeyAuth::precheck(&key).is_ok());
    }

    #[test]
    fn error_messages_never_contain_the_key() {
        let key = Secret::new("this-is-the-secret-key-value-1234");
        let err = ApiKeyAuth::precheck(&Secret::new("short")).unwrap_err();
        assert!(!format!("{err}").contains(key.expose()));
        // And the secret itself refuses to render.
        assert_eq!(format!("{key}"), onera_core::redact::REDACTED);
    }
}
