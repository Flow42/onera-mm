//! Contract tests for dependency ingestion against mocked HTTP fixtures.
//!
//! No test here touches the network or needs an API key. Every fixture is a
//! checked-in copy of a response shape the v3 specification documents, and the
//! behaviour each one pins down is written up in `docs/nexus-api-assumptions.md`.

use async_trait::async_trait;
use onera_core::domain::dependency::{
    CandidateStatus, DependencyAvailability, DependencyCapability, DependencySource,
    RequirementKind,
};
use onera_core::ids::{ProviderId, ProviderModId, ProviderVersionId};
use onera_core::ports::{AccountInfo, AuthProvider, Credential, ModProvider};
use onera_core::progress::CancelToken;
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};
use onera_nexus::{DependencyLimits, NexusClient, NexusConfig, RetryPolicy};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MATERIALIZED: &str = "/v3/mod-file-versions/dependencies/ranges/materialized/batch";
const DETAILS: &str = "/v3/mod-file-versions/batch";

struct StaticAuth;

#[async_trait]
impl AuthProvider for StaticAuth {
    fn provider_id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn is_authenticated(&self) -> Result<bool> {
        Ok(true)
    }
    async fn credential(&self) -> Result<Credential> {
        Ok(Credential::ApiKey(Secret::new("test-api-key-0123456789")))
    }
    async fn validate(&self, _c: &Credential) -> Result<AccountInfo> {
        unimplemented!("not used by these tests")
    }
    async fn store(&self, _c: Credential) -> Result<AccountInfo> {
        unimplemented!("not used by these tests")
    }
    async fn forget(&self) -> Result<()> {
        Ok(())
    }
}

fn client(server: &MockServer, retry: RetryPolicy, limits: DependencyLimits) -> NexusClient {
    let config = NexusConfig {
        v3_base: format!("{}/v3", server.uri()),
        v1_base: format!("{}/v1", server.uri()),
        retry,
        timeout: Duration::from_secs(5),
        dependency_limits: limits,
        ..NexusConfig::default()
    };
    NexusClient::new_for_tests(config, Arc::new(StaticAuth)).unwrap()
}

fn plain(server: &MockServer) -> NexusClient {
    client(server, RetryPolicy::none(), DependencyLimits::default())
}

/// A source keyed on a Nexus mod file version id, as the application supplies it.
fn source(version_id: &str) -> DependencySource {
    DependencySource {
        provider: ProviderId::nexus(),
        game_slug: "cyberpunk2077".into(),
        provider_mod_id: ProviderModId::new("107"),
        provider_file_id: None,
        provider_version_id: Some(ProviderVersionId::new(version_id)),
    }
}

/// One authored dependency definition pointing at a mod file on a mod.
fn definition(id: &str, mod_file_id: &str, mod_name: &str, domain: &str) -> Value {
    json!({
        "id": id,
        "ranges": [{
            "id": format!("{id}-r1"),
            "target_mod_file": {
                "id": mod_file_id,
                "name": "Main file",
                "mod": {
                    "id": "9000",
                    "game_scoped_id": "2165",
                    "name": mod_name,
                    "game": { "id": "3333", "name": "Cyberpunk 2077", "domain_name": domain }
                }
            },
            "min_version": { "id": "500", "position": "1", "name": "x", "version": "1.0" },
            "max_version": null
        }]
    })
}

fn declaration(definitions: Vec<Value>, dlc: Vec<Value>) -> Value {
    json!({ "dependency_definitions": definitions, "dlc_dependency_definitions": dlc })
}

fn candidate(source_id: &str, definition_id: &str, mod_file_id: &str, version_id: &str) -> Value {
    json!({
        "source_version_id": source_id,
        "definition_id": definition_id,
        "mod_file_id": mod_file_id,
        "version_id": version_id,
        "position": "2.5",
        "category": "main",
        "mod_status": "published",
        "mod_id": "12884901995"
    })
}

fn page(candidates: Vec<Value>, page: u32, page_size: u32, total: u64) -> Value {
    json!({
        "data": { "candidates": candidates },
        "meta": { "page": page, "page_size": page_size, "total_count": total }
    })
}

async fn mount_declaration(server: &MockServer, version_id: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/v3/mod-file-versions/{version_id}/dependencies"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_details(server: &MockServer, versions: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path(DETAILS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "versions": versions }
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn the_capability_is_declared_before_anything_is_asked() {
    let server = MockServer::start().await;
    assert_eq!(
        plain(&server).dependency_capability(),
        DependencyCapability::Supported {
            batch: true,
            dlc: true
        }
    );
}

#[tokio::test]
async fn and_definitions_become_groups_and_or_rows_become_candidates() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![
                definition("d1", "700", "Cyber Engine Tweaks", "cyberpunk2077"),
                definition("d2", "800", "RED4ext", "cyberpunk2077"),
            ],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                candidate("100", "d1", "700", "701"),
                // Same definition, same chain: an OR-alternative, older position.
                json!({
                    "source_version_id": "100", "definition_id": "d1",
                    "mod_file_id": "700", "version_id": "702", "position": "1.5",
                    "category": "old_version", "mod_status": "published", "mod_id": "1"
                }),
                candidate("100", "d2", "800", "801"),
            ],
            1,
            1000,
            3,
        )))
        .mount(&server)
        .await;
    mount_details(
        &server,
        vec![
            json!({"id":"701","mod_id":"1","mod_file_id":"700","name":"CET","version":"1.31","position":"2.5"}),
            json!({"id":"702","mod_id":"1","mod_file_id":"700","name":"CET 1.30","version":"1.30","position":"1.5"}),
            json!({"id":"801","mod_id":"2","mod_file_id":"800","name":"RED4ext","version":"1.2","position":"2.5"}),
        ],
    )
    .await;

    let snapshots = plain(&server)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.availability, DependencyAvailability::Fetched);
    assert!(!snapshot.declares_no_dependencies());
    assert_eq!(snapshot.groups.len(), 2, "two AND groups");
    assert!(snapshot
        .groups
        .iter()
        .all(|g| g.kind == RequirementKind::Required));
    assert_eq!(snapshot.groups[0].provider_group_key.as_deref(), Some("d1"));
    assert_eq!(
        snapshot.groups[0].label.as_deref(),
        Some("Cyber Engine Tweaks — Main file")
    );

    let or_group = &snapshot.groups[0];
    assert_eq!(or_group.candidates.len(), 2, "two OR alternatives");
    // Deterministic order: newest position within the chain first.
    assert_eq!(
        or_group.candidates[0]
            .provider_version_id
            .as_ref()
            .unwrap()
            .as_str(),
        "701"
    );
    let newest = &or_group.candidates[0];
    assert_eq!(newest.game_slug, "cyberpunk2077");
    assert_eq!(newest.provider_mod_id.as_str(), "2165");
    // Version identity, downloadable file and update chain stay distinct fields.
    assert_eq!(newest.provider_file_id.as_ref().unwrap().as_str(), "701");
    assert_eq!(
        newest.provider_file_group_id.as_ref().unwrap().as_str(),
        "700"
    );
    assert!(newest.position.unwrap() > or_group.candidates[1].position.unwrap());
    assert_eq!(newest.status, CandidateStatus::Available);
    assert_eq!(newest.display_name.as_deref(), Some("CET (1.31)"));
    assert!(!or_group.is_unsatisfiable("cyberpunk2077"));
    // A candidate for this game is not a candidate for another one.
    assert!(or_group.is_unsatisfiable("skyrimspecialedition"));

    // The raw provider response is preserved for diagnostics.
    assert!(snapshot.raw["declaration"]["dependency_definitions"].is_array());
    assert_eq!(
        snapshot.raw["materialized_candidates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(!snapshot.fingerprint.as_str().is_empty());
}

#[tokio::test]
async fn a_dependency_free_version_is_told_apart_from_zero_materialized_rows() {
    let server = MockServer::start().await;
    // 100 declares nothing at all.
    mount_declaration(&server, "100", declaration(vec![], vec![])).await;
    // 200 declares something the resolver cannot currently satisfy.
    mount_declaration(
        &server,
        "200",
        declaration(
            vec![definition("d9", "700", "A required mod", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![], 1, 1000, 0)))
        .mount(&server)
        .await;

    let snapshots = plain(&server)
        .dependencies(&[source("100"), source("200")], &CancelToken::new())
        .await
        .unwrap();

    // Positively dependency-free.
    assert!(snapshots[0].declares_no_dependencies());
    assert!(snapshots[0].groups.is_empty());

    // Declared but unresolvable: the group is kept with no candidates, so the
    // requirement stays visible instead of vanishing into "compatible".
    assert_eq!(snapshots[1].availability, DependencyAvailability::Fetched);
    assert!(!snapshots[1].declares_no_dependencies());
    assert_eq!(snapshots[1].groups.len(), 1);
    assert!(snapshots[1].groups[0].candidates.is_empty());
    assert!(snapshots[1].groups[0].is_unsatisfiable("cyberpunk2077"));
    assert_eq!(snapshots[1].blocking_groups().len(), 1);
}

#[tokio::test]
async fn dlc_alternatives_are_kept_as_or_alternatives() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![],
            vec![json!({
                "id": "dlc1",
                "dlc_targets": [
                    { "id": "t1", "dlc_id": "1234", "name": "Phantom Liberty" },
                    { "id": "t2", "dlc_id": "5678", "name": "Phantom Liberty (bundle)" }
                ]
            })],
        ),
    )
    .await;

    let snapshots = plain(&server)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.availability, DependencyAvailability::Fetched);
    // DLC is a requirement, so this version is not dependency-free.
    assert!(!snapshot.declares_no_dependencies());
    assert_eq!(snapshot.dlc.len(), 1);
    assert_eq!(snapshot.dlc[0].alternatives.len(), 2);
    assert_eq!(snapshot.dlc[0].alternatives[0].as_str(), "1234");
    assert_eq!(snapshot.dlc[0].label.as_deref(), Some("Phantom Liberty"));
}

#[tokio::test]
async fn hidden_removed_and_unknown_candidates_are_never_selectable() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    let row = |version: &str, status: &str, category: &str| {
        json!({
            "source_version_id": "100", "definition_id": "d1", "mod_file_id": "700",
            "version_id": version, "position": "1", "category": category,
            "mod_status": status, "mod_id": "1"
        })
    };
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                row("701", "hidden", "main"),
                row("702", "removed_by_staff", "main"),
                row("703", "a_status_from_the_future", "main"),
                row("704", "published", "removed"),
            ],
            1,
            1000,
            4,
        )))
        .mount(&server)
        .await;
    mount_details(&server, vec![]).await;

    let snapshots = plain(&server)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    let group = &snapshots[0].groups[0];
    let status_of = |version: &str| {
        group
            .candidates
            .iter()
            .find(|c| c.provider_version_id.as_ref().unwrap().as_str() == version)
            .unwrap()
            .status
    };
    assert_eq!(status_of("701"), CandidateStatus::Hidden);
    assert_eq!(status_of("702"), CandidateStatus::Removed);
    assert_eq!(status_of("703"), CandidateStatus::Unknown);
    assert_eq!(status_of("704"), CandidateStatus::Removed);
    // Four candidates, none of them installable.
    assert_eq!(group.candidates.len(), 4);
    assert!(group.is_unsatisfiable("cyberpunk2077"));
}

#[tokio::test]
async fn a_multi_page_batch_is_paginated_to_completion() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .and(body_partial_json(json!({ "page": 1 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![candidate("100", "d1", "700", "701")],
            1,
            1,
            3,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .and(body_partial_json(json!({ "page": 2 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![candidate("100", "d1", "700", "702")],
            2,
            1,
            3,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .and(body_partial_json(json!({ "page": 3 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![candidate("100", "d1", "700", "703")],
            3,
            1,
            3,
        )))
        .expect(1)
        .mount(&server)
        .await;
    mount_details(&server, vec![]).await;

    let limits = DependencyLimits {
        page_size: 1,
        ..DependencyLimits::default()
    };
    let snapshots = client(&server, RetryPolicy::none(), limits)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(
        snapshots[0].groups[0].candidates.len(),
        3,
        "every page must be collected"
    );
}

#[tokio::test]
async fn several_sources_are_answered_once_each_in_request_order() {
    let server = MockServer::start().await;
    for id in ["100", "200", "300"] {
        mount_declaration(
            &server,
            id,
            declaration(
                vec![definition("d1", "700", "Required", "cyberpunk2077")],
                vec![],
            ),
        )
        .await;
    }
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                candidate("300", "d1", "700", "703"),
                candidate("100", "d1", "700", "701"),
            ],
            1,
            1000,
            2,
        )))
        .mount(&server)
        .await;
    mount_details(&server, vec![]).await;

    // Deliberately out of order and with a repeat: the answer follows the
    // request, not the order Nexus happened to return rows in.
    let requested = vec![source("300"), source("100"), source("200"), source("300")];
    let snapshots = plain(&server)
        .dependencies(&requested, &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(snapshots.len(), 4);
    for (snapshot, wanted) in snapshots.iter().zip(&requested) {
        assert_eq!(&snapshot.source, wanted);
    }
    assert_eq!(snapshots[0].groups[0].candidates.len(), 1, "300");
    assert_eq!(snapshots[1].groups[0].candidates.len(), 1, "100");
    assert_eq!(
        snapshots[2].groups[0].candidates.len(),
        0,
        "200 got no rows"
    );
    assert_eq!(snapshots[3].groups[0].candidates.len(), 1, "300 repeated");
    // The repeated source is answered from the same fetch, not a second one.
    assert_eq!(
        snapshots[0].fingerprint, snapshots[3].fingerprint,
        "the same version must fingerprint identically"
    );
}

#[tokio::test]
async fn source_ids_are_chunked_to_the_configured_request_limit() {
    let server = MockServer::start().await;
    for id in ["100", "200"] {
        mount_declaration(
            &server,
            id,
            declaration(
                vec![definition("d1", "700", "Required", "cyberpunk2077")],
                vec![],
            ),
        )
        .await;
    }
    // One request per source, each carrying exactly its own id.
    for id in ["100", "200"] {
        Mock::given(method("POST"))
            .and(path(MATERIALIZED))
            .and(body_partial_json(json!({ "version_ids": [id] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(page(
                vec![candidate(id, "d1", "700", "701")],
                1,
                1000,
                1,
            )))
            .expect(1)
            .mount(&server)
            .await;
    }
    mount_details(&server, vec![]).await;

    let limits = DependencyLimits {
        max_sources_per_request: 1,
        ..DependencyLimits::default()
    };
    let snapshots = client(&server, RetryPolicy::none(), limits)
        .dependencies(&[source("100"), source("200")], &CancelToken::new())
        .await
        .unwrap();
    assert!(snapshots
        .iter()
        .all(|s| s.availability == DependencyAvailability::Fetched));
}

#[tokio::test]
async fn a_server_that_never_stops_paging_is_bounded_and_reported_honestly() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    // Always a full page and always "more to come".
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![candidate("100", "d1", "700", "701")],
            1,
            1,
            u64::from(u32::MAX),
        )))
        .mount(&server)
        .await;

    let limits = DependencyLimits {
        page_size: 1,
        max_pages: 3,
        ..DependencyLimits::default()
    };
    let snapshots = client(&server, RetryPolicy::none(), limits)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    let DependencyAvailability::Unavailable { reason } = &snapshots[0].availability else {
        panic!("expected unavailable, got {:?}", snapshots[0].availability);
    };
    assert!(reason.contains("3-page limit"), "{reason}");
    assert!(snapshots[0].groups.is_empty());
    assert!(!snapshots[0].declares_no_dependencies());
}

#[tokio::test]
async fn more_rows_than_the_row_ceiling_is_refused_rather_than_truncated() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                candidate("100", "d1", "700", "701"),
                candidate("100", "d1", "700", "702"),
                candidate("100", "d1", "700", "703"),
            ],
            1,
            1000,
            3,
        )))
        .mount(&server)
        .await;

    let limits = DependencyLimits {
        max_rows: 2,
        ..DependencyLimits::default()
    };
    let snapshots = client(&server, RetryPolicy::none(), limits)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    let DependencyAvailability::Unavailable { reason } = &snapshots[0].availability else {
        panic!("expected unavailable, got {:?}", snapshots[0].availability);
    };
    // A truncated candidate list would silently turn a satisfiable requirement
    // into an unsatisfiable one, so the whole source is reported unavailable.
    assert!(reason.contains("more than 2"), "{reason}");
}

#[tokio::test]
async fn a_throttled_batch_post_is_retried_with_the_same_body() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .and(body_partial_json(json!({ "version_ids": ["100"] })))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .and(body_partial_json(json!({ "version_ids": ["100"] })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-rl-hourly-remaining", "17")
                .set_body_json(page(vec![candidate("100", "d1", "700", "701")], 1, 1000, 1)),
        )
        .mount(&server)
        .await;
    // The real API sends the budget headers on every response, so the mock does
    // too: the client keeps the most recent reading, whichever call made it.
    Mock::given(method("POST"))
        .and(path(DETAILS))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-rl-hourly-remaining", "17")
                .set_body_json(json!({ "data": { "versions": [] } })),
        )
        .mount(&server)
        .await;

    let nexus = client(&server, RetryPolicy::default(), DependencyLimits::default());
    let snapshots = nexus
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(snapshots[0].availability, DependencyAvailability::Fetched);
    assert_eq!(snapshots[0].groups[0].candidates.len(), 1);
    // POST responses feed the same rate-limit accounting as GET.
    assert_eq!(nexus.rate_limit().hourly_remaining, Some(17));
}

#[tokio::test]
async fn cancellation_aborts_the_whole_call_rather_than_marking_sources() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .mount(&server)
        .await;

    let cancel = CancelToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });

    let started = std::time::Instant::now();
    let err = client(&server, RetryPolicy::default(), DependencyLimits::default())
        .dependencies(&[source("100")], &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_lost_credential_aborts_the_whole_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = plain(&server)
        .dependencies(&[source("100"), source("200")], &CancelToken::new())
        .await
        .unwrap_err();
    assert!(err.is_auth(), "{err:?}");
}

#[tokio::test]
async fn a_disappeared_declaration_endpoint_is_unavailable_not_dependency_free() {
    let server = MockServer::start().await;
    mount_declaration(&server, "100", declaration(vec![], vec![])).await;
    // 200's declaration endpoint has been withdrawn.
    Mock::given(method("GET"))
        .and(path("/v3/mod-file-versions/200/dependencies"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "title": "Not Found", "detail": "no such endpoint", "status": 404
        })))
        .mount(&server)
        .await;

    let snapshots = plain(&server)
        .dependencies(&[source("100"), source("200")], &CancelToken::new())
        .await
        .unwrap();

    assert!(snapshots[0].declares_no_dependencies());
    assert!(matches!(
        snapshots[1].availability,
        DependencyAvailability::Unavailable { .. }
    ));
    // The failure of one source does not contaminate the other.
    assert!(!snapshots[1].declares_no_dependencies());
    assert!(!snapshots[1].availability.is_authoritative());
}

#[tokio::test]
async fn a_disappeared_batch_endpoint_makes_only_the_affected_sources_unavailable() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    // 200 declares nothing, so it never needs the batch endpoint at all.
    mount_declaration(&server, "200", declaration(vec![], vec![])).await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let snapshots = plain(&server)
        .dependencies(&[source("100"), source("200")], &CancelToken::new())
        .await
        .unwrap();
    assert!(matches!(
        snapshots[0].availability,
        DependencyAvailability::Unavailable { .. }
    ));
    assert!(snapshots[0].groups.is_empty());
    assert!(snapshots[1].declares_no_dependencies());
}

#[tokio::test]
async fn malformed_responses_produce_unavailability_not_panics() {
    let server = MockServer::start().await;
    // Not JSON at all.
    Mock::given(method("GET"))
        .and(path("/v3/mod-file-versions/100/dependencies"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"))
        .mount(&server)
        .await;
    // JSON, but not the documented shape.
    Mock::given(method("GET"))
        .and(path("/v3/mod-file-versions/200/dependencies"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "dependency_definitions": "not an array" })),
        )
        .mount(&server)
        .await;
    // Well-formed declaration, malformed candidate page.
    mount_declaration(
        &server,
        "300",
        declaration(
            vec![definition("d1", "700", "Required", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": 42 })))
        .mount(&server)
        .await;

    let snapshots = plain(&server)
        .dependencies(
            &[source("100"), source("200"), source("300")],
            &CancelToken::new(),
        )
        .await
        .unwrap();
    for snapshot in &snapshots {
        assert!(
            matches!(
                snapshot.availability,
                DependencyAvailability::Unavailable { .. }
            ),
            "{:?}",
            snapshot.availability
        );
        assert!(!snapshot.declares_no_dependencies());
    }
}

#[tokio::test]
async fn a_candidate_whose_game_is_unknown_is_not_selectable() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![json!({ "id": "d1", "ranges": [{ "id": "r1" }] })],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![candidate("100", "d1", "700", "701")],
            1,
            1000,
            1,
        )))
        .mount(&server)
        .await;
    mount_details(&server, vec![]).await;

    let snapshots = plain(&server)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    let group = &snapshots[0].groups[0];
    // The row was kept, but nothing about it is invented.
    assert_eq!(group.candidates.len(), 1);
    assert_eq!(group.candidates[0].status, CandidateStatus::Unknown);
    assert_eq!(group.candidates[0].game_slug, "");
    assert!(group.is_unsatisfiable("cyberpunk2077"));
}

#[tokio::test]
async fn failed_hydration_costs_labels_and_nothing_else() {
    let server = MockServer::start().await;
    mount_declaration(
        &server,
        "100",
        declaration(
            vec![definition("d1", "700", "Required mod", "cyberpunk2077")],
            vec![],
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(MATERIALIZED))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![candidate("100", "d1", "700", "701")],
            1,
            1000,
            1,
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(DETAILS))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let snapshots = plain(&server)
        .dependencies(&[source("100")], &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(snapshots[0].availability, DependencyAvailability::Fetched);
    let candidate = &snapshots[0].groups[0].candidates[0];
    assert_eq!(candidate.status, CandidateStatus::Available);
    assert_eq!(
        candidate.provider_version_id.as_ref().unwrap().as_str(),
        "701"
    );
    // Falls back to the declaration's own label rather than showing nothing.
    assert_eq!(
        candidate.display_name.as_deref(),
        Some("Required mod — Main file")
    );
}

#[tokio::test]
async fn a_source_without_a_version_identity_is_unavailable_and_costs_no_request() {
    let server = MockServer::start().await;
    let mut without = source("100");
    without.provider_version_id = None;

    let snapshots = plain(&server)
        .dependencies(&[without], &CancelToken::new())
        .await
        .unwrap();
    assert!(matches!(
        snapshots[0].availability,
        DependencyAvailability::Unavailable { .. }
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn hostile_version_identifiers_cannot_escape_their_path_segment() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/mod-file-versions/..%2F..%2Fadmin/dependencies"))
        .respond_with(ResponseTemplate::new(200).set_body_json(declaration(vec![], vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let snapshots = plain(&server)
        .dependencies(&[source("../../admin")], &CancelToken::new())
        .await
        .unwrap();
    assert!(snapshots[0].declares_no_dependencies());
}

#[tokio::test]
async fn an_empty_request_asks_nothing() {
    let server = MockServer::start().await;
    let snapshots = plain(&server)
        .dependencies(&[], &CancelToken::new())
        .await
        .unwrap();
    assert!(snapshots.is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}
