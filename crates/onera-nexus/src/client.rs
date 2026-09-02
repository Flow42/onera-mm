//! The Nexus Mods HTTP client.
//!
//! One `reqwest::Client` is shared for connection pooling. Every request goes
//! through one private `send` helper, which is the single place that applies
//! authentication, retries, rate-limit accounting, cancellation and error
//! mapping — so no endpoint can forget one of them.

use crate::error::{map_status, parse_problem};
use crate::models::*;
use crate::retry::{parse_retry_after, RateLimit, RetryPolicy};
use async_trait::async_trait;
use onera_core::domain::dependency::{DependencyCapability, DependencySnapshot, DependencySource};
use onera_core::domain::game::Game;
use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::{GameId, ModId, ProviderFileId, ProviderId, ProviderModId, ReleaseId};
use onera_core::ports::{AuthProvider, Credential, DownloadTarget, ModProvider, Page};
use onera_core::progress::CancelToken;
use onera_core::redact::redact_url;
use onera_core::{CoreError, Result};
use serde::de::DeserializeOwned;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default production base URL for API v3.
pub const DEFAULT_V3_BASE: &str = "https://api.nexusmods.com/v3";
/// Base URL for the endpoints v3 does not yet cover. See
/// `docs/nexus-api-assumptions.md`.
pub const DEFAULT_V1_BASE: &str = "https://api.nexusmods.com/v1";

/// Header the personal-API-key scheme uses, per the v3 specification.
const API_KEY_HEADER: &str = "apikey";

/// Largest response body Onera will read from Nexus.
///
/// A batch dependency page is a few hundred kilobytes. Reading is bounded rather
/// than trusted because a hostile or broken server that answers a small request
/// with an endless body would otherwise exhaust memory before any parser sees it.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Configuration for the client.
#[derive(Debug, Clone)]
pub struct NexusConfig {
    /// Base URL for v3 endpoints.
    pub v3_base: String,
    /// Base URL for compatibility endpoints.
    pub v1_base: String,
    /// How Onera identifies itself. Nexus requires a real user agent.
    pub user_agent: String,
    /// Retry pacing.
    pub retry: RetryPolicy,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Bounds on the dependency batch endpoints.
    pub dependency_limits: DependencyLimits,
}

/// Bounds applied to the paginated dependency batch endpoints.
///
/// Every one of these is a refusal to trust a remote server about how much work
/// it may ask Onera to do. The defaults are the documented request limits; the
/// page and row ceilings exist so a server that keeps reporting "one more page"
/// produces an honest `Unavailable` rather than an infinite loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyLimits {
    /// Source version ids per materialized batch request. The specification
    /// caps `version_ids` at 5000.
    pub max_sources_per_request: usize,
    /// Version ids per version-detail batch request. The specification caps it
    /// at 2000.
    pub max_details_per_request: usize,
    /// Candidate rows requested per page. The specification caps it at 5000.
    pub page_size: u32,
    /// Pages Onera will fetch for one chunk before giving up.
    pub max_pages: u32,
    /// Candidate rows Onera will hold for one chunk before giving up.
    pub max_rows: usize,
}

impl Default for DependencyLimits {
    fn default() -> Self {
        Self {
            max_sources_per_request: 5000,
            max_details_per_request: 2000,
            page_size: 1000,
            max_pages: 64,
            max_rows: 100_000,
        }
    }
}

impl DependencyLimits {
    /// The limits with every zero replaced by a usable minimum.
    ///
    /// A zero page size would ask for empty pages forever, and a zero chunk size
    /// would never send a request at all; both are configuration mistakes rather
    /// than reasons to hang.
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            max_sources_per_request: self.max_sources_per_request.clamp(1, 5000),
            max_details_per_request: self.max_details_per_request.clamp(1, 2000),
            page_size: self.page_size.clamp(1, 5000),
            max_pages: self.max_pages.max(1),
            max_rows: self.max_rows.max(1),
        }
    }
}

impl Default for NexusConfig {
    fn default() -> Self {
        Self {
            v3_base: DEFAULT_V3_BASE.to_owned(),
            v1_base: DEFAULT_V1_BASE.to_owned(),
            user_agent: format!(
                "Onera/{} (+https://github.com/onera-mm/onera)",
                env!("CARGO_PKG_VERSION")
            ),
            retry: RetryPolicy::default(),
            timeout: Duration::from_secs(30),
            dependency_limits: DependencyLimits::default(),
        }
    }
}

/// A typed Nexus Mods client.
pub struct NexusClient {
    http: reqwest::Client,
    config: NexusConfig,
    auth: Arc<dyn AuthProvider>,
    rate_limit: Arc<Mutex<RateLimit>>,
    /// Whether a plain-HTTP download location is acceptable. Only ever true in
    /// tests pointed at a local mock server.
    allow_plain_http: bool,
}

impl NexusClient {
    /// Build a client.
    ///
    /// # Errors
    /// Fails if the HTTP stack cannot be initialized.
    pub fn new(config: NexusConfig, auth: Arc<dyn AuthProvider>) -> Result<Self> {
        Self::build(config, auth, true)
    }

    /// Build a client that will also talk to a plain-HTTP server.
    ///
    /// Exists only so the contract tests can point the real client at a local
    /// mock server. Production code calls [`NexusClient::new`], which refuses
    /// anything but HTTPS.
    ///
    /// # Errors
    /// Fails if the HTTP stack cannot be initialized.
    pub fn new_for_tests(config: NexusConfig, auth: Arc<dyn AuthProvider>) -> Result<Self> {
        Self::build(config, auth, false)
    }

    fn build(config: NexusConfig, auth: Arc<dyn AuthProvider>, https_only: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .timeout(config.timeout)
            // Redirects are followed, but only a bounded number and only to
            // https; see `safe_redirect_policy` in onera-download for the
            // download path, which is stricter still.
            .redirect(reqwest::redirect::Policy::limited(5))
            .https_only(https_only)
            .build()
            .map_err(|e| CoreError::Provider(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            http,
            config,
            auth,
            rate_limit: Arc::new(Mutex::new(RateLimit::default())),
            allow_plain_http: !https_only,
        })
    }

    /// The most recent rate-limit reading.
    #[must_use]
    pub fn rate_limit(&self) -> RateLimit {
        *self.rate_limit.lock().expect("rate limit mutex poisoned")
    }

    /// Perform a `GET` and deserialize the body.
    ///
    /// Applies authentication, retries with jittered backoff, rate-limit
    /// accounting and cancellation.
    ///
    /// # Errors
    /// Returns a mapped [`CoreError`] for any non-success status, or
    /// [`CoreError::Provider`] if the body does not deserialize.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        cancel: &CancelToken,
    ) -> Result<T> {
        let body = self.send(url, None, cancel).await?;
        Self::decode(url, &body)
    }

    /// Perform an authenticated JSON `POST` and deserialize the body.
    ///
    /// Shares every guarantee of [`NexusClient::get_json`] — timeout, retry with
    /// jittered backoff, rate-limit accounting, cancellation, redaction and
    /// error mapping — because both go through the same `send`. The request body
    /// is serialized once and replayed byte for byte on each attempt, so a retry
    /// can never send a different request from the one that failed.
    ///
    /// # Errors
    /// Returns a mapped [`CoreError`] for any non-success status, or
    /// [`CoreError::Provider`] if the request cannot be serialized or the
    /// response does not deserialize.
    pub async fn post_json<B: serde::Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        url: &str,
        payload: &B,
        cancel: &CancelToken,
    ) -> Result<T> {
        let encoded = serde_json::to_string(payload).map_err(|e| {
            CoreError::Provider(format!(
                "cannot encode request for {}: {e}",
                redact_url(url)
            ))
        })?;
        let body = self.send(url, Some(&encoded), cancel).await?;
        Self::decode(url, &body)
    }

    fn decode<T: DeserializeOwned>(url: &str, body: &str) -> Result<T> {
        serde_json::from_str(body).map_err(|e| {
            // The URL is logged redacted: query strings can carry credentials.
            CoreError::Provider(format!("unreadable response from {}: {e}", redact_url(url)))
        })
    }

    async fn send(&self, url: &str, payload: Option<&str>, cancel: &CancelToken) -> Result<String> {
        let mut attempt = 0_u32;
        loop {
            cancel.check()?;
            match self.attempt(url, payload).await {
                Ok(body) => return Ok(body),
                Err(error) => {
                    if !self.config.retry.should_retry(attempt, &error) {
                        return Err(error);
                    }
                    let hint = match &error {
                        CoreError::RateLimited {
                            retry_after_secs, ..
                        } => Some(Duration::from_secs(*retry_after_secs)),
                        _ => None,
                    };
                    let jitter: f64 = rand::random();
                    let delay = self.config.retry.delay_for(attempt, hint, jitter);
                    tracing::debug!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        url = %redact_url(url),
                        "retrying nexus request"
                    );
                    // Cancellation must win over a long rate-limit backoff, or
                    // pressing Cancel would appear to do nothing for a minute.
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = wait_for_cancel(cancel) => return Err(CoreError::Cancelled),
                    }
                    attempt += 1;
                }
            }
        }
    }

    async fn attempt(&self, url: &str, payload: Option<&str>) -> Result<String> {
        let mut request = match payload {
            Some(body) => self
                .http
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_owned()),
            None => self.http.get(url),
        };
        match self.auth.credential().await? {
            Credential::ApiKey(key) => {
                request = request.header(API_KEY_HEADER, key.expose());
            }
            Credential::Bearer(token) => {
                request = request.bearer_auth(token.expose());
            }
        }

        let response = request.send().await.map_err(|e| {
            CoreError::Provider(format!(
                "request to {} failed: {}",
                redact_url(url),
                redact_url(&e.to_string())
            ))
        })?;

        let status = response.status();
        let headers = response.headers().clone();
        *self.rate_limit.lock().expect("rate limit mutex poisoned") =
            RateLimit::from_headers(&headers);

        let body = read_bounded(response, url).await?;

        if status.is_success() {
            return Ok(body);
        }
        Err(map_status(
            status.as_u16(),
            parse_problem(&body),
            parse_retry_after(&headers).map(|d| d.as_secs()),
        ))
    }

    fn v3(&self, path: &str) -> String {
        format!("{}{path}", self.config.v3_base.trim_end_matches('/'))
    }

    /// A v3 URL, for the sibling modules that build their own paths.
    pub(crate) fn v3_url(&self, path: &str) -> String {
        self.v3(path)
    }

    /// The configured dependency bounds, with any nonsense value corrected.
    pub(crate) fn dependency_limits(&self) -> DependencyLimits {
        self.config.dependency_limits.sanitized()
    }

    fn v1(&self, path: &str) -> String {
        format!("{}{path}", self.config.v1_base.trim_end_matches('/'))
    }
}

/// Read a response body, refusing to buffer more than [`MAX_RESPONSE_BYTES`].
///
/// The size is enforced while streaming rather than after the fact, so an
/// oversized body is abandoned instead of being allocated and then rejected.
/// The failure is [`CoreError::Unsupported`] and therefore not retried: asking
/// the same endpoint again would produce the same oversized answer.
async fn read_bounded(mut response: reqwest::Response, url: &str) -> Result<String> {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|e| CoreError::Provider(format!("cannot read response body: {e}")))?;
        let Some(chunk) = chunk else { break };
        if buffer.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(CoreError::Unsupported(format!(
                "response from {} exceeds the {MAX_RESPONSE_BYTES}-byte limit",
                redact_url(url)
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    String::from_utf8(buffer)
        .map_err(|_| CoreError::Provider(format!("response from {} is not UTF-8", redact_url(url))))
}

/// Resolve when the token is cancelled.
///
/// Polling rather than a notification primitive keeps [`CancelToken`] free of an
/// async dependency, which matters because the same token is checked from the
/// blocking archive worker.
async fn wait_for_cancel(cancel: &CancelToken) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[async_trait]
impl ModProvider for NexusClient {
    fn id(&self) -> ProviderId {
        ProviderId::nexus()
    }

    async fn games(&self, cursor: Option<&str>, cancel: &CancelToken) -> Result<Page<Game>> {
        // v3 does not expose a game catalogue; see docs/nexus-api-assumptions.md
        // for why the v1 endpoint is used and what happens when it goes away.
        let _ = cursor;
        let url = self.v1("/games.json");
        let wire: Vec<WireGame> = self.get_json(&url, cancel).await?;
        let items = wire
            .into_iter()
            .map(|g| Game {
                id: GameId::new(),
                provider: ProviderId::nexus(),
                provider_slug: g.domain,
                name: g.name.unwrap_or_else(|| "Unnamed game".to_owned()),
                steam_app_id: None,
            })
            .collect();
        Ok(Page::single(items))
    }

    async fn mod_metadata(
        &self,
        game_slug: &str,
        mod_id: &ProviderModId,
        cancel: &CancelToken,
    ) -> Result<(Mod, Vec<Release>)> {
        let url = self.v3(&format!(
            "/games/{}/mods/{}",
            urlencode(game_slug),
            urlencode(mod_id.as_str())
        ));
        let envelope: Envelope<WireMod> = self.get_json(&url, cancel).await?;
        let wire = envelope.data;

        let the_mod = Mod {
            id: ModId::new(),
            provider: ProviderId::nexus(),
            provider_mod_id: mod_id.clone(),
            game_slug: game_slug.to_owned(),
            name: wire.name.unwrap_or_else(|| format!("Mod {mod_id}")),
            author: wire.author,
        };

        // Releases come from the mod's file versions: a release is one published
        // version string with the date Nexus recorded for it.
        let versions = self.file_versions(&wire.id, cancel).await?;
        let releases = versions
            .iter()
            .map(|v| Release {
                id: ReleaseId::new(),
                mod_id: the_mod.id,
                // Stored exactly as reported. Never parsed.
                version: v
                    .version
                    .clone()
                    .unwrap_or_else(|| "unversioned".to_owned()),
                published_at: v.uploaded_at,
                metadata: serde_json::json!({
                    "nexus_mod_file_version_id": v.id,
                    "nexus_game_scoped_id": v.game_scoped_id,
                }),
            })
            .collect();

        Ok((the_mod, releases))
    }

    async fn files(
        &self,
        game_slug: &str,
        mod_id: &ProviderModId,
        cursor: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<Page<ProviderFile>> {
        let _ = cursor;
        let url = self.v3(&format!(
            "/games/{}/mods/{}",
            urlencode(game_slug),
            urlencode(mod_id.as_str())
        ));
        let envelope: Envelope<WireMod> = self.get_json(&url, cancel).await?;
        let versions = self.file_versions(&envelope.data.id, cancel).await?;

        let items = versions
            .into_iter()
            .map(|v| {
                let provider_file_group_id = v
                    .file
                    .as_ref()
                    .map(|file| onera_core::ids::ProviderFileGroupId::new(file.id.clone()));
                ProviderFile {
                    provider: ProviderId::nexus(),
                    provider_file_id: ProviderFileId::new(v.id.clone()),
                    provider_version_id: Some(onera_core::ids::ProviderVersionId::new(v.id)),
                    provider_file_group_id,
                    position: None,
                    // Filled in by the caller once the release is persisted; the
                    // provider does not own Onera's release identity.
                    release_id: ReleaseId::new(),
                    name: v.name.unwrap_or_else(|| "download".to_owned()),
                    size_bytes: v.size,
                    category: map_category(v.category),
                    published_hash: v
                        .md5_hash
                        .as_deref()
                        .and_then(|h| FileHash::md5_from_hex(h).ok()),
                    uploaded_at: v.uploaded_at,
                    is_primary: v.is_primary.unwrap_or(false),
                }
            })
            .collect();
        Ok(Page::single(items))
    }

    async fn resolve_download(
        &self,
        game_slug: &str,
        mod_id: &ProviderModId,
        file_id: &ProviderFileId,
        cancel: &CancelToken,
    ) -> Result<DownloadTarget> {
        // Download resolution is not in the v3 specification Onera was built
        // against; the documented v1 endpoint is used until it is.
        let url = self.v1(&format!(
            "/games/{}/mods/{}/files/{}/download_link.json",
            urlencode(game_slug),
            urlencode(mod_id.as_str()),
            urlencode(file_id.as_str())
        ));
        let links: Vec<DownloadLink> = self.get_json(&url, cancel).await?;
        let first = links.into_iter().next().ok_or_else(|| {
            CoreError::Provider(
                "Nexus returned no download locations; a free account may need to start the download from the website".to_owned(),
            )
        })?;
        let parsed = require_safe_download_url(&first.uri, self.allow_plain_http)?;

        Ok(DownloadTarget {
            url: parsed,
            headers: Vec::new(),
            expected_size: None,
            filename: file_id.to_string(),
        })
    }

    fn dependency_capability(&self) -> DependencyCapability {
        // Nexus models both halves: version-range dependencies resolvable in one
        // batch request, and store DLC requirements.
        DependencyCapability::Supported {
            batch: true,
            dlc: true,
        }
    }

    async fn dependencies(
        &self,
        sources: &[DependencySource],
        cancel: &CancelToken,
    ) -> Result<Vec<DependencySnapshot>> {
        // See `crate::dependencies` for why this needs three endpoints and why
        // a missing candidate row is never read as "nothing required".
        self.fetch_dependencies(sources, cancel).await
    }
}

impl NexusClient {
    /// Every file version across every file slot of a mod.
    async fn file_versions(
        &self,
        nexus_mod_id: &str,
        cancel: &CancelToken,
    ) -> Result<Vec<WireModFileVersion>> {
        let files_url = self.v3(&format!("/mods/{}/files", urlencode(nexus_mod_id)));
        let files: Envelope<WireModFilesResponse> = self.get_json(&files_url, cancel).await?;

        let mut out = Vec::new();
        for file in files.data.mod_files {
            cancel.check()?;
            let versions_url = self.v3(&format!("/mod-files/{}/versions", urlencode(&file.id)));
            let mut versions: Envelope<WireVersionsResponse> =
                self.get_json(&versions_url, cancel).await?;
            for version in &mut versions.data.versions {
                // Some responses omit the back-reference even though the outer
                // request already established the exact file/update chain.
                // Carry that opaque provider identity forward; never recreate
                // it from the version string or display name.
                version.file.get_or_insert_with(|| file.clone());
            }
            out.extend(versions.data.versions);
        }
        Ok(out)
    }
}

/// A v1 download location.
#[derive(Debug, Clone, serde::Deserialize)]
struct DownloadLink {
    #[serde(alias = "URI")]
    uri: String,
}

/// Accept a download location only if it is safe to fetch.
///
/// A provider that hands back a `http://` or `file://` location is either
/// misconfigured or hostile; either way the bytes must not be fetched. The
/// `allow_plain_http` escape exists only for tests against a local mock server
/// and is never true in a shipped binary.
fn require_safe_download_url(raw: &str, allow_plain_http: bool) -> Result<url::Url> {
    let parsed = url::Url::parse(raw).map_err(|e| {
        CoreError::Provider(format!("Nexus returned an unusable download URL: {e}"))
    })?;
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if allow_plain_http => Ok(parsed),
        other => Err(CoreError::Provider(format!(
            "refusing a non-HTTPS download location (scheme {other:?})"
        ))),
    }
}

fn map_category(category: WireCategory) -> FileCategory {
    match category {
        WireCategory::Main => FileCategory::Main,
        WireCategory::Update => FileCategory::Update,
        WireCategory::Optional => FileCategory::Optional,
        WireCategory::OldVersion => FileCategory::OldVersion,
        WireCategory::Miscellaneous => FileCategory::Miscellaneous,
        WireCategory::Removed | WireCategory::Archived | WireCategory::Unknown => {
            FileCategory::Unknown
        }
    }
}

/// Percent-encode a single path segment.
///
/// Mod and game identifiers come from a browser extension and are therefore
/// untrusted; a slug containing `../` must not be able to reach a different
/// endpoint.
pub(crate) fn urlencode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(urlencode("cyberpunk2077"), "cyberpunk2077");
        assert_eq!(urlencode("../admin"), "..%2Fadmin");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("héllo"), "h%C3%A9llo");
    }

    #[test]
    fn categories_map_onto_the_domain_enum() {
        assert_eq!(map_category(WireCategory::Main), FileCategory::Main);
        assert_eq!(map_category(WireCategory::Unknown), FileCategory::Unknown);
        // Removed and archived files are downloadable in principle but must not
        // be presented as ordinary options.
        assert_eq!(map_category(WireCategory::Removed), FileCategory::Unknown);
    }

    #[test]
    fn only_https_download_locations_are_accepted() {
        // This is the production rule: every shipped constructor passes false.
        assert!(require_safe_download_url("https://cdn.example.test/f.zip", false).is_ok());
        for hostile in [
            "http://insecure.example.test/f.zip",
            "file:///etc/passwd",
            "ftp://example.test/f.zip",
            "not a url",
        ] {
            let err = require_safe_download_url(hostile, false).unwrap_err();
            assert!(
                format!("{err}").contains("refusing") || format!("{err}").contains("unusable"),
                "{hostile} produced {err}"
            );
        }
        // Only the test escape hatch relaxes it, and only for http.
        assert!(require_safe_download_url("http://127.0.0.1:8080/f.zip", true).is_ok());
        assert!(require_safe_download_url("file:///etc/passwd", true).is_err());
    }

    #[test]
    fn the_default_config_targets_v3_over_https() {
        let config = NexusConfig::default();
        assert!(config.v3_base.starts_with("https://"));
        assert!(config.v3_base.ends_with("/v3"));
        assert!(config.user_agent.contains("Onera/"));
    }
}
