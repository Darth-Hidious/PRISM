//! Smoke test: spawn FunctionGemma, route a real query, verify we get a
//! structured tool_call back.
//!
//! Marked `#[ignore]` like embed_smoke.rs — requires the Q4_K_M FunctionGemma
//! GGUF at ~/.prism/models/functiongemma-270m.gguf. Run explicitly:
//!   cargo test -p prism_tool_router --test route_smoke -- --ignored

use std::path::PathBuf;

use prism_tool_router::{Config, RoutingDecision, ToolRouter};
use serde_json::json;

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

#[tokio::test]
#[ignore]
async fn function_router_emits_call() {
    let config = Config::default_for_home(&home());
    let router = ToolRouter::new(config).await.expect("new");
    router
        .start_function_router()
        .await
        .expect("function router");

    let tools = vec![json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get the current weather for a city.",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": { "type": "string", "description": "The city to check" }
                },
                "required": ["city"]
            }
        }
    })];

    let decision = router
        .route("What's the weather like in Paris?", &tools)
        .await;

    match decision {
        RoutingDecision::Invoke(call) => {
            assert_eq!(call.name, "get_weather");
            let city = call
                .arguments
                .get("city")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert!(
                city.eq_ignore_ascii_case("paris"),
                "expected city=Paris, got {city:?}; full args = {:?}",
                call.arguments
            );
        }
        RoutingDecision::Passthrough => panic!("expected tool call, got passthrough"),
    }

    router.shutdown().await;
}
