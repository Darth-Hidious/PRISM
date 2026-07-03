//! Smoke test: spawn the real EmbeddingGemma subprocess, embed two sentences,
//! verify shape + that semantically related strings are closer than unrelated.
//!
//! Marked `#[ignore]` so it doesn't run in normal `cargo test` (would always
//! require a 320 MB model on disk + a free port + 30s of subprocess time).
//! Run explicitly:  cargo test -p prism_tool_router --test embed_smoke -- --ignored

use std::path::PathBuf;

use prism_tool_router::{Config, ToolDef, ToolRouter};

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap())
}

#[tokio::test]
#[ignore]
async fn embed_then_search() {
    let config = Config::default_for_home(&home());
    let router = ToolRouter::new(config).await.expect("new");
    router.start().await.expect("start subprocess");

    let tools = vec![
        ToolDef {
            name: "search_materials".into(),
            description: "Search a knowledge graph for materials matching given properties.".into(),
            args_schema: serde_json::json!({}),
        },
        ToolDef {
            name: "compile_code".into(),
            description: "Compile a Rust crate using cargo.".into(),
            args_schema: serde_json::json!({}),
        },
        ToolDef {
            name: "send_email".into(),
            description: "Send an email message via SMTP.".into(),
            args_schema: serde_json::json!({}),
        },
    ];
    let n = router.index_tools(&tools).await.expect("index");
    assert_eq!(n, 3);

    let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    let top = router
        .search("find me Inconel 718 mechanical properties", &names, 1)
        .await
        .expect("search");
    assert_eq!(top, vec!["search_materials".to_string()]);

    let top = router
        .search("how do I write a Rust async function", &names, 1)
        .await
        .expect("search");
    assert_eq!(top, vec!["compile_code".to_string()]);

    router.shutdown().await;
}
