# Adding a mod provider

Nothing in Onera's installation domain knows what Nexus Mods is. A second
provider — Mod.io, GameBanana, a local directory — is two trait implementations.

## The traits

```rust
pub trait ModProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn games(&self, cursor: Option<&str>, cancel: &CancelToken) -> Result<Page<Game>>;
    async fn mod_metadata(&self, game_slug: &str, mod_id: &ProviderModId, cancel: &CancelToken)
        -> Result<(Mod, Vec<Release>)>;
    async fn files(&self, game_slug: &str, mod_id: &ProviderModId, cursor: Option<&str>,
                   cancel: &CancelToken) -> Result<Page<ProviderFile>>;
    async fn resolve_download(&self, game_slug: &str, mod_id: &ProviderModId,
                              file_id: &ProviderFileId, cancel: &CancelToken)
        -> Result<DownloadTarget>;
}
```

`ProviderModId` and `ProviderFileId` are opaque strings. Whatever your service
uses as a key — an integer, a UUID, a slug — becomes one of these and is never
interpreted by anything downstream.

## Authentication is separate

```rust
pub trait AuthProvider: Send + Sync {
    async fn is_authenticated(&self) -> Result<bool>;
    async fn credential(&self) -> Result<Credential>;
    async fn validate(&self, credential: &Credential) -> Result<AccountInfo>;
    async fn store(&self, credential: Credential) -> Result<AccountInfo>;
    async fn forget(&self) -> Result<()>;
}
```

Splitting authentication from the provider is what lets Nexus SSO replace the
personal API key later without touching `NexusClient`. Implement it separately
even if your service only has one mechanism today.

Two rules your `store` must honour:

1. **Validate before storing.** An invalid credential must never reach the
   secret store, or "authenticated" and "has a stored key" drift apart.
2. **No plaintext fallback.** If `SecretStore::is_available` is false, fail. A
   mod manager that quietly writes a credential to `~/.config` when the keyring
   is locked is worse than one that refuses.

## Checklist

**Treat every response as untrusted.** Non-required fields are `Option`. Enums
get a `#[serde(other)]` fallback so a new server-side value cannot break a
listing. Error bodies are parsed defensively and truncated before display.

**Percent-encode path segments.** Identifiers may come from a browser extension.
A mod id of `../../admin` must not reach a different endpoint. `onera-nexus` has
a small `urlencode` with tests you can copy.

**Route everything through one send function.** Authentication, retries,
rate-limit accounting, cancellation and error mapping belong in one place, or
some endpoint will forget one of them.

**Map errors onto `CoreError`.** `Unauthenticated`, `RateLimited`, `NotFound`,
`InvalidInput` and `Provider` all mean something specific to the UI —
`is_retryable()` and `is_auth()` drive real behaviour.

**Honour cancellation during backoff.** A `tokio::select!` between the sleep and
the cancel token; otherwise pressing Cancel appears to do nothing for a minute.

**Never scrape.** If the data is not in the API, Onera does without it.

## Registering

```rust
let provider: Arc<dyn ModProvider> = Arc::new(YourClient::new(config, auth.clone())?);
let onera = Onera::assemble(paths, auth, provider).await?;
```

Then seed a row in `providers` and use the slug consistently — it is the primary
key that scopes games, mods and files.

## Testing

Mock the HTTP layer with `wiremock` and cover, at minimum: a happy path,
pagination, a rate limit that succeeds on retry, a rate limit that exhausts its
attempts, a client error that must _not_ be retried, cancellation during
backoff, a malformed body, a response missing a required field, and a hostile
identifier. `crates/onera-nexus/tests/api_contract.rs` is a working template.

Default tests must need no network and no credential.
