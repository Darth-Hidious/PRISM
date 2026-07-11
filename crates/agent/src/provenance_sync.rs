//! Fail-open sync of local agent provenance to the MARC27 platform.
//!
//! PRISM records every tool call / LLM turn to the local
//! `~/.prism/provenance.db`. This mirrors each record to the platform's
//! `recordAgentProvenance` GraphQL mutation so the trail is visible online +
//! on the platform, not just on the machine that produced it. Best-effort:
//! any failure is logged at debug and dropped, never blocking or failing the
//! local write. The tenant is enforced server-side from the auth token — this
//! only ever writes into the authenticated caller's own trail.

use prism_client::PlatformClient;
use prism_provenance::ProvenanceRecord;
use prism_runtime::{PlatformEndpoints, PrismPaths};
use tracing::debug;

const MUTATION: &str = "mutation($records: [AgentProvenanceRecordInput!]!) \
     { recordAgentProvenance(records: $records) }";

/// Sync is ON by default (the platform is where provenance should be visible);
/// hard-off with `PRISM_PROVENANCE_SYNC=0` (or `false`/`off`).
fn enabled() -> bool {
    !matches!(
        std::env::var("PRISM_PROVENANCE_SYNC").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// `(api_base, token)` for the platform, or `None` when not authenticated.
/// A non-expiring `MARC27_API_KEY` is preferred over the session JWT.
fn resolve_auth() -> Option<(String, String)> {
    let api_base = PlatformEndpoints::from_env().api_base;
    if let Ok(key) = std::env::var("MARC27_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some((api_base, key));
        }
    }
    let creds = PrismPaths::discover()
        .ok()?
        .load_cli_state()
        .ok()?
        .credentials?;
    if creds.access_token.is_empty() {
        return None;
    }
    Some((api_base, creds.access_token))
}

/// Map a local record onto the platform's camelCase `AgentProvenanceRecordInput`.
/// `input_json`/`output_json` are stringified (the platform stores them as text).
fn record_to_input(r: &ProvenanceRecord) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "sessionId": r.session_id,
        "timestamp": r.timestamp,
        "actionType": serde_json::to_value(&r.action_type).unwrap_or(serde_json::Value::Null),
        "actor": serde_json::to_value(&r.actor).unwrap_or(serde_json::Value::Null),
        "toolName": r.tool_name,
        "llmModel": r.llm_model,
        "inputJson": serde_json::to_string(&r.input_json).ok(),
        "outputJson": r.output_json.as_ref().and_then(|v| serde_json::to_string(v).ok()),
        "parentId": r.parent_id,
        "materialRef": r.material_ref,
        "confidence": r.confidence,
        "tags": r.tags,
    })
}

/// Best-effort mirror of one record to the platform. Swallows every error.
pub async fn try_push(record: &ProvenanceRecord) {
    if !enabled() {
        return;
    }
    let Some((api_base, token)) = resolve_auth() else {
        return;
    };
    let body = serde_json::json!({
        "query": MUTATION,
        "variables": { "records": [record_to_input(record)] },
    });
    let client = PlatformClient::new(api_base).with_token(token);
    match client
        .post::<serde_json::Value, serde_json::Value>("/graphql", &body)
        .await
    {
        // GraphQL returns 200 even on resolver errors — surface them at debug.
        Ok(v) => {
            if let Some(errs) = v.get("errors") {
                debug!("provenance sync: platform returned errors: {errs}");
            }
        }
        Err(e) => debug!("provenance sync failed (dropped): {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_provenance::{ActionType, Actor, new_record};

    #[test]
    fn maps_record_to_camel_case_input_with_stringified_json() {
        let mut r = new_record(
            "sess-1",
            ActionType::ToolCall,
            Actor::Agent,
            Some("query_platform"),
            None,
            serde_json::json!({ "q": "titanium" }),
        );
        r.output_json = Some(serde_json::json!({ "hits": 3 }));
        let v = record_to_input(&r);
        assert_eq!(v["sessionId"], "sess-1");
        assert_eq!(v["actionType"], "tool_call");
        assert_eq!(v["actor"], "agent");
        assert_eq!(v["toolName"], "query_platform");
        // JSON payloads are sent as strings (platform stores them as text).
        assert_eq!(v["inputJson"], "{\"q\":\"titanium\"}");
        assert_eq!(v["outputJson"], "{\"hits\":3}");
    }
}
