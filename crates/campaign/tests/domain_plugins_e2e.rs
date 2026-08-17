//! End-to-end evidence that alloy and polymer plugins use the same campaign
//! loop. Both external boundaries are deterministic localhost fakes; no model,
//! materials API, database, compute service, or remote network is contacted.

use std::ffi::OsString;

use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

use prism_campaign::{
    ALLOY_DOMAIN_ID, Campaign, CampaignConfig, CampaignGoal, EvidenceClass, POLYMER_DOMAIN_ID,
};

const FOX_FLORY_CITATION: &str = "T. G. Fox and P. J. Flory, Journal of Applied Physics 21 (1950) 581-591, DOI 10.1063/1.1699711";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct HomeGuard {
    previous: Option<OsString>,
}

impl HomeGuard {
    fn install_fake_identity(home: &std::path::Path) -> Self {
        let previous = std::env::var_os("HOME");
        // SAFETY: every test in this integration-test process holds ENV_LOCK.
        unsafe { std::env::set_var("HOME", home) };
        let paths = prism_runtime::PrismPaths::discover().unwrap();
        paths
            .save_cli_state(&prism_runtime::PrismCliState {
                credentials: Some(prism_runtime::StoredCredentials {
                    user_id: Some("domain-e2e-user".into()),
                    display_name: Some("Domain E2E User".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap();
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: every test in this integration-test process holds ENV_LOCK.
        unsafe {
            match &self.previous {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

async fn create_session() -> Json<Value> {
    Json(json!({"session_id": "domain-e2e-session"}))
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some("Bearer domain-e2e-session")
}

async fn evaluate_alloy(headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "missing deterministic test session"})),
        );
    }
    let composition = body["inputs"]["composition"].as_str().unwrap_or("");
    (
        StatusCode::OK,
        Json(json!({
            "tool": "hea_descriptors",
            "result": {"result": {
                "composition": composition,
                "Tm_estimate_K": 3200.0,
                "delta_S_mix_J_per_molK": 8.314 * 4.0_f64.ln(),
                "fractions": [0.25, 0.25, 0.25, 0.25],
                "method": "deterministic test boundary for existing alloy path",
                "evidence_class": "screening",
                "evidence_color": "yellow"
            }}
        })),
    )
}

async fn evaluate_polymer(
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "missing deterministic test session"})),
        );
    }
    let identity_text = body["inputs"]["candidate_identity"].as_str().unwrap_or("");
    let identity: Value = serde_json::from_str(identity_text).unwrap();
    let parameters = &identity["fox_flory"];
    let mn = parameters["number_average_molar_mass_g_per_mol"]
        .as_f64()
        .unwrap();
    let tg_infinity = parameters["tg_infinity_k"].as_f64().unwrap();
    let constant = parameters["k_k_g_per_mol"].as_f64().unwrap();
    let tg = tg_infinity - constant / mn;
    (
        StatusCode::OK,
        Json(json!({
            "tool": "polymer_insulation_properties",
            "result": {"result": {
                "glass_transition_temperature_k": tg,
                "property_status": {
                    "glass_transition_temperature_k": {
                        "status": "computed",
                        "value": tg,
                        "unit": "K",
                        "method": "Fox-Flory molecular-weight relation: Tg = Tg_infinity - K/Mn",
                        "citation": FOX_FLORY_CITATION,
                        "parameter_citation": parameters["parameter_citation"],
                        "evidence_class": "screening",
                        "evidence_color": "yellow"
                    },
                    "dielectric_constant": {
                        "status": "unavailable",
                        "reason": "No citable dielectric-constant method is implemented for this candidate representation; measured data or a separately validated model is required."
                    },
                    "dielectric_breakdown_strength_kv_per_mm": {
                        "status": "unavailable",
                        "reason": "No validated dielectric-breakdown-strength method is implemented. PRISM will not estimate a high-voltage safety property from identity alone."
                    },
                    "thermal_conductivity_w_per_m_k": {
                        "status": "unavailable",
                        "reason": "No citable thermal-conductivity method is implemented for this candidate representation; morphology and measurement evidence are required."
                    }
                },
                "evidence_class": "screening",
                "evidence_color": "yellow"
            }}
        })),
    )
}

async fn spawn_app(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

async fn spawn_boundary() -> String {
    spawn_app(
        axum::Router::new()
            .route("/api/sessions", post(create_session))
            .route("/api/tools/hea_descriptors/run", post(evaluate_alloy))
            .route(
                "/api/tools/polymer_insulation_properties/run",
                post(evaluate_polymer),
            ),
    )
    .await
}

fn config(base: String, checkpoint_dir: &std::path::Path, domain: &str) -> CampaignConfig {
    CampaignConfig {
        domain: domain.to_string(),
        max_iterations: 1,
        batch_size: 1,
        checkpoint_every: 1,
        checkpoint_dir: Some(checkpoint_dir.to_path_buf()),
        node_base_url: Some(base),
        ..Default::default()
    }
}

#[tokio::test]
async fn nbmotaw_alloy_campaign_transcript_is_unchanged() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install_fake_identity(home.path());
    let checkpoint_dir = tempfile::tempdir().unwrap();
    let base = spawn_boundary().await;
    let goal = CampaignGoal {
        description: "Find a refractory high-entropy alloy".into(),
        elements: ["Nb", "Mo", "Ta", "W"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        objective: "maximize melting point".into(),
        // CONTRACT CHANGE: the reward property is declared, not parsed from
        // the objective's English words.
        target_property: Some("Tm_estimate_K".into()),
        constraints: Vec::new(),
        seeds: vec!["NbMoTaW".into()],
    };
    let mut campaign = Campaign::new(
        goal,
        config(base, checkpoint_dir.path(), ALLOY_DOMAIN_ID),
        "domain-e2e-nbmotaw".into(),
    );
    let result = campaign.run().await.unwrap();

    println!("--- NbMoTaW alloy campaign ---\n{}", result.summary);
    assert_eq!(result.winners[0].composition, "Nb0.25 Mo0.25 Ta0.25 W0.25");
    assert_eq!(result.winners[0].reward, 3200.0);
    assert_eq!(result.winners[0].evidence_class, EvidenceClass::Screening);
    assert_eq!(result.evidence_class, EvidenceClass::Screening);
    assert!(
        result.summary.contains("[YELLOW screening]"),
        "{}",
        result.summary
    );
    assert_eq!(
        result.winners[0].properties["hea_definition"]["name"],
        "permissive_rhea"
    );

    let checkpoint = checkpoint_dir.path().join("domain-e2e-nbmotaw.json");
    let checkpoint_value: Value =
        serde_json::from_str(&std::fs::read_to_string(&checkpoint).unwrap()).unwrap();
    assert_eq!(checkpoint_value["evidence_class"], "screening");
    assert_eq!(
        checkpoint_value["candidates"][0]["evidence_class"],
        "screening"
    );
}

#[tokio::test]
async fn absent_polymer_plugin_reports_the_rdkit_install_hint() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install_fake_identity(home.path());
    let checkpoint_dir = tempfile::tempdir().unwrap();
    let base = spawn_app(axum::Router::new().route("/api/sessions", post(create_session))).await;
    let goal = CampaignGoal {
        description: "Check polymer plugin availability".into(),
        elements: Vec::new(),
        objective: "maximize glass transition temperature".into(),
        // CONTRACT CHANGE: the reward property is declared, not parsed from
        // the objective's English words.
        target_property: Some("glass_transition_temperature_k".into()),
        constraints: Vec::new(),
        seeds: vec![
            json!({
                "representation": "monomer",
                "monomer": "ethylene"
            })
            .to_string(),
        ],
    };
    let mut campaign = Campaign::new(
        goal,
        config(base, checkpoint_dir.path(), POLYMER_DOMAIN_ID),
        "domain-e2e-polymer-unavailable".into(),
    );
    let error = campaign.run().await.unwrap_err();
    let message = format!("{error:#}");

    println!("--- Polymer plugin unavailable ---\n{message}");
    assert!(message.contains("Domain unavailable"), "{message}");
    assert!(message.contains("python -m pip install rdkit"), "{message}");
}

#[tokio::test]
async fn polymer_campaign_computes_only_cited_tg_and_reports_other_targets_unavailable() {
    let _lock = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install_fake_identity(home.path());
    let checkpoint_dir = tempfile::tempdir().unwrap();
    let base = spawn_boundary().await;
    let seed = json!({
        "representation": "repeat_unit",
        "name": "customer-parameterized-polymer",
        "repeat_unit": "[-R-]",
        "fox_flory": {
            "number_average_molar_mass_g_per_mol": 50000.0,
            "tg_infinity_k": 450.0,
            "k_k_g_per_mol": 100000.0,
            "parameter_citation": "customer-supplied demonstration parameters; not a PRISM estimate"
        }
    })
    .to_string();
    let goal = CampaignGoal {
        description: "Screen a polymer candidate for electrical insulation".into(),
        elements: Vec::new(),
        objective: "maximize glass transition temperature".into(),
        // CONTRACT CHANGE: the reward property is declared, not parsed from
        // the objective's English words.
        target_property: Some("glass_transition_temperature_k".into()),
        constraints: Vec::new(),
        seeds: vec![seed],
    };
    let mut campaign = Campaign::new(
        goal,
        config(base, checkpoint_dir.path(), POLYMER_DOMAIN_ID),
        "domain-e2e-polymer".into(),
    );
    let result = campaign.run().await.unwrap();
    let properties = &result.winners[0].properties;

    println!("--- Polymer campaign ---\n{}", result.summary);
    println!(
        "Property evidence:\n{}",
        serde_json::to_string_pretty(&properties["property_status"]).unwrap()
    );
    assert_eq!(properties["glass_transition_temperature_k"], 448.0);
    assert_eq!(result.winners[0].evidence_class, EvidenceClass::Screening);
    assert_eq!(result.evidence_class, EvidenceClass::Screening);
    assert!(
        result.summary.contains("[YELLOW screening]"),
        "{}",
        result.summary
    );
    assert_eq!(
        properties["property_status"]["glass_transition_temperature_k"]["citation"],
        FOX_FLORY_CITATION
    );
    for property in [
        "dielectric_constant",
        "dielectric_breakdown_strength_kv_per_mm",
        "thermal_conductivity_w_per_m_k",
    ] {
        assert_eq!(
            properties["property_status"][property]["status"],
            "unavailable"
        );
        assert!(
            properties["property_status"][property]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
        assert!(properties.get(property).is_none());
    }

    let checkpoint = checkpoint_dir.path().join("domain-e2e-polymer.json");
    let mut legacy_checkpoint: Value =
        serde_json::from_str(&std::fs::read_to_string(&checkpoint).unwrap()).unwrap();
    legacy_checkpoint
        .as_object_mut()
        .unwrap()
        .remove("evidence_class");
    legacy_checkpoint["candidates"][0]
        .as_object_mut()
        .unwrap()
        .remove("evidence_class");
    std::fs::write(
        &checkpoint,
        serde_json::to_vec_pretty(&legacy_checkpoint).unwrap(),
    )
    .unwrap();

    let restored = Campaign::from_checkpoint(&checkpoint).unwrap();
    assert_eq!(restored.state().config.domain, POLYMER_DOMAIN_ID);
    assert_eq!(restored.state().evidence_class, EvidenceClass::Screening);
    assert_eq!(
        restored.state().candidates[0].properties["glass_transition_temperature_k"],
        448.0
    );
    assert_eq!(
        restored.state().candidates[0].evidence_class,
        EvidenceClass::Screening
    );
}
