//! Opt-in compatibility smoke test against the real Nexus Mods API.
//!
//! Every other test in this crate runs against recorded fixtures, which is what
//! keeps `cargo test --workspace` free of network access and credentials. That
//! is the right default, and it has one blind spot: a fixture cannot notice
//! that the live API stopped matching it. `nexus_openapi.yaml` marks the
//! dependency endpoints experimental, so drift there is expected rather than
//! hypothetical.
//!
//! These tests close that gap without weakening the default. They are
//! `#[ignore]`d, so a normal run never selects them, and they skip themselves
//! when no key is present, so even `--ignored` is safe on a machine without
//! credentials. **CI never needs a secret for this suite.**
//!
//! ```sh
//! export ONERA_LIVE_NEXUS_KEY=...                   # a personal API key
//! cargo test -p onera-nexus --test live_smoke -- --ignored --test-threads=1
//! ```
//!
//! To refresh the offline fixtures from live responses at the same time:
//!
//! ```sh
//! ONERA_LIVE_NEXUS_RECORD=crates/onera-nexus/tests/fixtures/live \
//!   cargo test -p onera-nexus --test live_smoke -- --ignored --test-threads=1
//! ```
//!
//! The recorded bodies are what a future contract fixture should be built from,
//! so a drift found here becomes a permanent offline test rather than a
//! one-time observation. **Read a recording before committing it**: a response
//! is tied to the account that fetched it and can carry personal fields.
//!
//! `--test-threads=1` matters. These share one account's rate-limit budget.

use onera_core::domain::dependency::{DependencyAvailability, DependencyCapability};
use onera_core::ids::{ProviderId, ProviderModId};
use onera_core::ports::{AccountInfo, AuthProvider, Credential, ModProvider};
use onera_core::progress::CancelToken;
use onera_core::redact::Secret;
use onera_nexus::{NexusClient, NexusConfig};
use std::sync::Arc;

/// An auth provider holding one key, so the smoke test needs no keyring.
///
/// The key comes from the environment and is wrapped in [`Secret`] immediately,
/// which is what keeps it out of logs and error text.
struct StaticAuth(Secret);

#[async_trait::async_trait]
impl AuthProvider for StaticAuth {
    fn provider_id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn is_authenticated(&self) -> onera_core::Result<bool> {
        Ok(true)
    }
    async fn credential(&self) -> onera_core::Result<Credential> {
        Ok(Credential::ApiKey(self.0.clone()))
    }
    async fn validate(&self, _: &Credential) -> onera_core::Result<AccountInfo> {
        unimplemented!("the smoke test does not validate through this path")
    }
    async fn store(&self, _: Credential) -> onera_core::Result<AccountInfo> {
        unimplemented!("the smoke test never stores a credential")
    }
    async fn forget(&self) -> onera_core::Result<()> {
        Ok(())
    }
}

/// Environment variable holding the personal API key.
const KEY_VAR: &str = "ONERA_LIVE_NEXUS_KEY";
/// Environment variable naming a directory to record raw responses into.
const RECORD_VAR: &str = "ONERA_LIVE_NEXUS_RECORD";

/// The game and mod the smoke test reads.
///
/// A long-lived, widely mirrored mod, so the test is not measuring one
/// author's decision to unpublish. Overridable because that can still happen.
fn subject() -> (String, ProviderModId) {
    let domain =
        std::env::var("ONERA_LIVE_NEXUS_GAME").unwrap_or_else(|_| "cyberpunk2077".to_owned());
    let mod_id = std::env::var("ONERA_LIVE_NEXUS_MOD").unwrap_or_else(|_| "107".to_owned());
    (domain, ProviderModId::new(&mod_id))
}

/// The API key, or `None` when the suite should skip.
fn key() -> Option<String> {
    match std::env::var(KEY_VAR) {
        Ok(key) if !key.trim().is_empty() => Some(key),
        _ => {
            eprintln!("skipping: set {KEY_VAR} to run the live compatibility smoke test");
            None
        }
    }
}

/// A client backed by the key in the environment, talking to the real API.
fn client(key: &str) -> NexusClient {
    let auth = StaticAuth(Secret::new(key));
    NexusClient::new(NexusConfig::default(), Arc::new(auth)).expect("client builds")
}

/// Write a raw response body next to the others, when recording is on.
fn record(name: &str, body: &str) {
    let Ok(dir) = std::env::var(RECORD_VAR) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("recording directory is writable");
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, body).expect("recording is writable");
    eprintln!("recorded {}", path.display());
}

/// Fetch one endpoint raw, for recording. Deliberately separate from the typed
/// client: a fixture has to capture what the server sent, not what Onera made
/// of it.
async fn raw(key: &str, url: &str) -> Option<String> {
    if std::env::var(RECORD_VAR).is_err() {
        return None;
    }
    let response = reqwest::Client::new()
        .get(url)
        .header("apikey", key)
        .header("user-agent", NexusConfig::default().user_agent)
        .send()
        .await
        .ok()?;
    response.text().await.ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The typed model still parses a live mod and its files.
///
/// This is the test that fails when the API changes shape underneath the
/// fixtures. It asserts only what Onera actually depends on — a mod has a name
/// and an id, a file has an id and a size — rather than pinning fields that
/// Nexus is free to change.
#[tokio::test]
#[ignore = "requires a live Nexus API key; see the module documentation"]
async fn live_mod_metadata_still_matches_the_typed_model() {
    let Some(key) = key() else { return };
    let (domain, mod_id) = subject();
    let cancel = CancelToken::new();

    let (the_mod, releases) = client(&key)
        .mod_metadata(&domain, &mod_id, &cancel)
        .await
        .expect("live mod metadata parses into the typed model");

    assert_eq!(the_mod.provider, ProviderId::nexus());
    assert_eq!(the_mod.provider_mod_id, mod_id);
    assert!(!the_mod.name.trim().is_empty(), "a mod must have a name");
    assert!(
        !releases.is_empty(),
        "a published mod must expose at least one release"
    );

    if let Some(body) = raw(
        &key,
        &format!("https://api.nexusmods.com/v3/mods/{}", mod_id.as_str()),
    )
    .await
    {
        record("mod_metadata", &body);
    }
}

#[tokio::test]
#[ignore = "requires a live Nexus API key; see the module documentation"]
async fn live_file_listing_still_matches_the_typed_model() {
    let Some(key) = key() else { return };
    let (domain, mod_id) = subject();
    let cancel = CancelToken::new();

    let page = client(&key)
        .files(&domain, &mod_id, None, &cancel)
        .await
        .expect("live file listing parses into the typed model");

    assert!(!page.items.is_empty(), "a published mod must expose files");
    for file in &page.items {
        assert!(
            !file.provider_file_id.as_str().is_empty(),
            "every file needs an id Onera can download by"
        );
    }

    if let Some(body) = raw(
        &key,
        &format!(
            "https://api.nexusmods.com/v3/mods/{}/files",
            mod_id.as_str()
        ),
    )
    .await
    {
        record("mod_files", &body);
    }
}

/// The experimental dependency endpoints, and the rule that losing them is not
/// fatal.
///
/// `docs/nexus-api-assumptions.md` records that these are marked experimental
/// upstream. If they disappear or change, Onera must degrade to "dependency
/// information unavailable" — a distinct, honest state — and never to "no
/// dependencies reported", which would let an unsatisfied install through.
#[tokio::test]
#[ignore = "requires a live Nexus API key; see the module documentation"]
async fn dependency_capability_loss_is_reported_not_fatal() {
    let Some(key) = key() else { return };
    let (domain, mod_id) = subject();
    let cancel = CancelToken::new();
    let client = client(&key);

    // The client advertises support. Whether the server still provides it is
    // exactly what this test is here to find out.
    assert!(
        matches!(
            client.dependency_capability(),
            DependencyCapability::Supported { .. }
        ),
        "the Nexus client should advertise dependency support"
    );

    let (_, releases) = client
        .mod_metadata(&domain, &mod_id, &cancel)
        .await
        .expect("mod metadata");
    assert!(
        !releases.is_empty(),
        "a published mod must expose a release to ask about"
    );

    let sources = vec![onera_core::domain::dependency::DependencySource {
        provider: ProviderId::nexus(),
        game_slug: domain.clone(),
        provider_mod_id: mod_id.clone(),
        provider_file_id: None,
        provider_version_id: None,
    }];

    // A transport failure is a legitimate outcome here and is reported as such;
    // what must never happen is a silent empty answer.
    match client.dependencies(&sources, &cancel).await {
        Ok(snapshots) => {
            assert_eq!(
                snapshots.len(),
                sources.len(),
                "exactly one snapshot per requested source"
            );
            let snapshot = &snapshots[0];
            match &snapshot.availability {
                DependencyAvailability::Fetched => {
                    eprintln!(
                        "dependency endpoints live: {} group(s)",
                        snapshot.groups.len()
                    );
                }
                DependencyAvailability::Unavailable { reason } => {
                    // Not a failure. This is the degraded state working.
                    eprintln!("dependency endpoints unavailable, reported honestly: {reason}");
                    assert!(
                        !reason.trim().is_empty(),
                        "an unavailable snapshot must say why"
                    );
                    assert!(
                        snapshot.groups.is_empty(),
                        "an unavailable snapshot must not also claim groups"
                    );
                }
                DependencyAvailability::Cached { fetched_at, .. } => {
                    // A live call must not be served from a cache; if it were,
                    // this test would not be exercising the API at all.
                    panic!("a live dependency request returned cached data from {fetched_at}");
                }
                DependencyAvailability::Unsupported => {
                    panic!("the Nexus client must never report its own support as unsupported");
                }
            }
        }
        Err(error) => {
            let message = error.to_string();
            assert!(
                !message.contains(&key),
                "the API key leaked into an error message"
            );
            eprintln!("dependency request failed, which is a reportable state: {message}");
        }
    }
}

/// A bad key must fail as an authentication error, and the key must not appear
/// in the message.
///
/// The offline tests assert this against a mock. Doing it live confirms the
/// real error body — which Onera does not control — is scrubbed too.
#[tokio::test]
#[ignore = "requires a live Nexus API key; see the module documentation"]
async fn a_rejected_key_never_appears_in_the_error() {
    if key().is_none() {
        return;
    }
    let bogus = "onera-live-smoke-definitely-not-a-real-key-0123456789";
    let (domain, mod_id) = subject();

    let error = client(bogus)
        .mod_metadata(&domain, &mod_id, &CancelToken::new())
        .await
        .expect_err("a bogus key must be refused");

    let message = format!("{error}");
    assert!(
        !message.contains(bogus),
        "the rejected key appeared in the error: {message}"
    );
}
