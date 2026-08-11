//! End-to-end mesh ARRIVAL: two PRISM servers in one process, separate
//! state dirs, loopback transport. Machine B ingests and publishes;
//! machine A pulls with the owner's platform token and B's facts land in
//! A's store under B's tenant, carrying B's origin.
//!
//! # What is real and what is stubbed
//!
//! Real: B's full router (`build_router` behind `start_server`) with the
//! production `auth_stack`, the session mint (`POST /api/sessions`), the
//! RBAC role lookup for the VERIFIED user, the `/api/query` serve path,
//! and A's entire pull (`sync_dataset_from_peer` + `PeerSessions`).
//!
//! Stubbed: the MARC27 platform. `create_session` verifies a presented
//! `platform_token` by calling `fetch_current_user` against the node's
//! linked platform base URL — the stub answers `/users/me` for exactly one
//! token, so the REAL verification code path runs; only the far end of the
//! HTTPS call is substituted. Decided before writing the test: a live
//! platform would make this suite depend on network + credentials.
//!
//! # What loopback CANNOT prove here
//!
//! `session_gate` refuses tokenless REMOTE callers, but both ends of this
//! test are 127.0.0.1, where a tokenless caller legitimately mints an
//! anonymous-local session. That refusal is unit-pinned in
//! `handlers/sessions.rs`. What THIS test pins instead: presenting a wrong
//! platform token fails the pull with a 401 — so the verification path is
//! demonstrably consulted, not bypassed.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;

const OWNER_TOKEN: &str = "owner-platform-token";
const OWNER_ID: &str = "owner-1";

/// Stub platform: verifies exactly [`OWNER_TOKEN`] on `/users/me` and
/// counts how often verification was actually consulted.
async fn spawn_platform_stub() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_in_route = hits.clone();
    let app = axum::Router::new().route(
        "/users/me",
        get(move |headers: HeaderMap| {
            let hits = hits_in_route.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let authorized = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v == format!("Bearer {OWNER_TOKEN}"));
                if authorized {
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "id": OWNER_ID,
                            "email": "owner@example.org",
                            "display_name": "Owner",
                        })),
                    )
                        .into_response()
                } else {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({ "error": "invalid token" })),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub platform");
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, hits)
}

/// Machine B: a full PRISM server with its own session/RBAC/provenance
/// stores, linked to the stub platform, mesh-online under `node_id`.
async fn spawn_node_b(
    dir: &std::path::Path,
    platform_base: &str,
    node_id: uuid::Uuid,
) -> std::net::SocketAddr {
    // The daemon's role sync writes the owner's org role into local RBAC;
    // the test performs that write directly (Engineer carries QueryData).
    let rbac_db = dir.join("rbac.db");
    let engine = prism_core::rbac::RbacEngine::new(&rbac_db).expect("open rbac");
    engine
        .assign_role(OWNER_ID, prism_core::rbac::LocalRole::Engineer)
        .expect("assign synced role");

    let mut state = prism_server::NodeState::new("machine-b".into());
    state.session_db_path = Some(dir.join("sessions.db"));
    state.rbac_db_path = Some(rbac_db);
    state.provenance_db_path = Some(dir.join("provenance-b.db"));
    state.platform_client =
        Some(prism_client::PlatformClient::new(platform_base).with_token("node-owner-cred"));
    state.identity_verifier = Some(
        prism_client::auth::IdentityVerifierConfig::new(
            Some(prism_client::auth::MARC27_IDENTITY_PROVIDER),
            platform_base,
            None,
        )
        .expect("configure MARC27 identity verifier"),
    );
    *state.mesh.write().unwrap() = prism_mesh::init_mesh_with_id(
        prism_mesh::MeshConfig {
            node_name: "machine-b".into(),
            publish_port: 0,
            discovery: vec![prism_mesh::DiscoveryMethod::Mdns],
            kafka_brokers: None,
        },
        node_id,
    )
    .expect("mesh online");

    let (addr, _handle) = prism_server::start_server(Arc::new(state), 0)
        .await
        .expect("start node B");
    addr
}

/// Ingest one fact on B the way `prism ingest` does: tenant "local",
/// origin `doc:test` — the origin A must still see after the pull.
async fn ingest_on_b(store_path: &std::path::Path) {
    let store = prism_provenance::ProvenanceStore::open(store_path)
        .await
        .expect("open B store");
    let now = chrono::Utc::now().to_rfc3339();
    let prov = prism_provenance::LocalProvenance {
        activity_id: "act_b_ingest".into(),
        agent_id: "prism-ingest".into(),
        agent_kind: "SoftwareAgent".into(),
        source_entity_id: "doc:test".into(),
        source_kind: "Document".into(),
        tenant: "local".into(),
        started_at: now.clone(),
        ended_at: now,
        locality: "local".into(),
        origin_source_id: None,
    };
    store.record_activity(&prov).await.expect("record activity");
    store
        .write_fact(
            &prism_provenance::LocalFact {
                subject: "Ti-6Al-4V".into(),
                predicate: "hasPhase".into(),
                object: "alpha phase".into(),
                value: None,
                unit: None,
                confidence: Some(0.9),
                kind: Some("phase".into()),
            },
            &prov,
        )
        .await
        .expect("ingest fact on B");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_lands_bs_facts_under_bs_tenant_with_bs_origin() {
    let dir = tempfile::tempdir().expect("state dir");
    let (platform_base, verify_hits) = spawn_platform_stub().await;
    let b_id = uuid::Uuid::new_v4();
    let b_addr = spawn_node_b(dir.path(), &platform_base, b_id).await;
    let b_base = format!("http://{b_addr}");
    ingest_on_b(&dir.path().join("provenance-b.db")).await;

    let http = prism_mesh::sync::sync_http_client();

    // The peer's public discovery route reports the node id `prism mesh
    // sync --peer <url>` keys the tenant on.
    let mesh_nodes: serde_json::Value = http
        .get(format!("{b_base}/api/mesh/nodes"))
        .send()
        .await
        .expect("reach B")
        .json()
        .await
        .expect("mesh nodes json");
    assert_eq!(
        mesh_nodes["node_id"].as_str(),
        Some(b_id.to_string().as_str()),
        "B must report its persistent mesh identity"
    );

    // B publishes the dataset (loopback anonymous session — the operator
    // acting on their own node), mirroring `prism mesh publish`.
    let session: serde_json::Value = http
        .post(format!("{b_base}/api/sessions"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("mint local session")
        .json()
        .await
        .expect("session json");
    let publish = http
        .post(format!("{b_base}/api/mesh/publish"))
        .header("X-Session-Token", session["session_id"].as_str().unwrap())
        .json(&serde_json::json!({ "name": "Ti-6Al-4V" }))
        .send()
        .await
        .expect("publish");
    assert!(publish.status().is_success(), "publish must succeed");

    // ── The arrival: A pulls from B with the OWNER's platform token over an
    // address vouched for by the authenticated platform registry. Merely
    // typing a peer URL is not identity proof and deliberately gets a
    // tokenless mint. ──
    let a_store = dir.path().join("provenance-a.db");
    let cfg = Some(prism_mesh::sync::SyncConfig {
        provenance_db: a_store.clone(),
    });
    let b_peer = prism_mesh::peer_session::PeerAddress {
        url: b_base.clone(),
        trust: prism_mesh::PeerTrust::PlatformRegistry,
    };
    let sessions = prism_mesh::peer_session::PeerSessions::new(Some(OWNER_TOKEN.into()));
    let synced = prism_mesh::sync::sync_dataset_from_peer(
        &http,
        &b_peer,
        "Ti-6Al-4V",
        b_id,
        &cfg,
        &sessions,
    )
    .await
    .expect("the pull must arrive");
    assert!(synced >= 1, "at least the queried entity must sync");
    assert!(
        verify_hits.load(Ordering::SeqCst) >= 1,
        "B must have verified the platform token against the platform"
    );

    // B's facts are in A's store, under B's OWN tenant…
    let tenant = prism_mesh::sync::mesh_tenant(&b_id);
    let a = prism_provenance::ProvenanceStore::open(&a_store)
        .await
        .expect("open A store");
    let nodes = a
        .graph_search("Ti-6Al-4V", &tenant, 10)
        .await
        .expect("search A store");
    assert!(
        nodes
            .iter()
            .any(|n| n.name == "Ti-6Al-4V" && n.entity_type == "Matter"),
        "B's entity must arrive typed under {tenant}: {nodes:?}"
    );
    // …and NOT under the old shared tenant or A's local tenant.
    for wrong_tenant in ["mesh", "local"] {
        let stray = a.graph_search("Ti-6Al-4V", wrong_tenant, 10).await.unwrap();
        assert!(
            stray.is_empty(),
            "synced data must not blend into tenant {wrong_tenant:?}: {stray:?}"
        );
    }

    // …carrying B's origin as relay evidence (`doc:test`, mesh-namespaced).
    let evidence = a
        .assertion_evidence(&tenant, "Ti-6Al-4V", "SYNCED_FROM", "Ti-6Al-4V")
        .await
        .expect("read A evidence");
    assert_eq!(
        evidence.len(),
        1,
        "one pull, one evidence row: {evidence:?}"
    );
    assert_eq!(
        evidence[0].source_key, "mesh:opaque:doc:test",
        "the origin B conveyed must key A's evidence"
    );

    // ── The gate: a WRONG platform token cannot pull ──
    // This is what proves verification is consulted rather than bypassed:
    // were the token ignored (anonymous fallback), this pull would succeed.
    let bad_sessions = prism_mesh::peer_session::PeerSessions::new(Some("wrong-token".into()));
    let err = prism_mesh::sync::sync_dataset_from_peer(
        &http,
        &b_peer,
        "Ti-6Al-4V",
        b_id,
        &cfg,
        &bad_sessions,
    )
    .await
    .expect_err("a wrong platform token must not arrive");
    assert!(
        err.to_string().contains("401"),
        "the refusal must surface as a 401, not an empty sync: {err}"
    );
}
