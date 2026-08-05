//! Tool invocation handlers.

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use crate::NodeState;
use crate::handlers::deployments::command_tool_platform_access;
use crate::middleware::AuthenticatedUser;

#[derive(Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub commands: Vec<ToolCommandInfo>,
}

#[derive(Serialize)]
pub struct ToolCommandInfo {
    pub name: String,
    pub description: String,
    pub args: Vec<ToolArgInfo>,
}

#[derive(Serialize)]
pub struct ToolArgInfo {
    pub name: String,
    pub arg_type: String,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// GET /api/tools — list available tools from the registry.
pub async fn list_tools(State(state): State<Arc<NodeState>>) -> Json<Vec<ToolInfo>> {
    let registry = state
        .tool_registry
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let tools = registry
        .list()
        .iter()
        .map(|entry| ToolInfo {
            name: entry.manifest.name.clone(),
            description: entry.manifest.description.clone(),
            version: entry.manifest.version.clone(),
            commands: entry
                .manifest
                .commands
                .iter()
                .map(|cmd| ToolCommandInfo {
                    name: cmd.name.clone(),
                    description: cmd.description.clone(),
                    args: cmd
                        .args
                        .iter()
                        .map(|a| ToolArgInfo {
                            name: a.name.clone(),
                            arg_type: a.arg_type.clone(),
                            required: a.required,
                            description: a.description.clone(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    Json(tools)
}

/// Build the args object a tool actually receives from the request body.
///
/// Three envelope forms are understood (checked in order):
///   * `{"args": { … }}`                    — explicit args, used verbatim
///     (relay-style / generic callers).
///   * `{"inputs": { … }, "command": "x"}`  — workflow `action: tool` shape:
///     `inputs` becomes the args and a non-null `command` is folded in as a
///     `command` kwarg (the Python tool server calls `tool.execute(**args)`,
///     so `command` is just another kwarg).
///   * a bare object `{ … }`                — the whole body is the args.
///
/// Anything that is not one of these (e.g. a JSON array/string body) yields an
/// empty args object; the tool then errors honestly on the missing inputs.
fn build_tool_args(body: &Value) -> Value {
    if let Some(args) = body.get("args") {
        return args.clone();
    }

    let has_envelope = body.get("inputs").is_some() || body.get("command").is_some();
    if !has_envelope {
        // No envelope keys → treat the entire object as the args, minus the
        // `approve` envelope flag (it's for the handler, not the tool).
        return if let Value::Object(map) = body {
            let mut args = map.clone();
            args.remove("approve");
            Value::Object(args)
        } else {
            Value::Object(Default::default())
        };
    }

    let mut args = match body.get("inputs") {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => Value::Object(Default::default()),
    };
    if let (Value::Object(map), Some(cmd)) = (&mut args, body.get("command"))
        && !cmd.is_null()
    {
        map.entry("command".to_string())
            .or_insert_with(|| cmd.clone());
    }
    args
}

/// POST /api/tools/:name/run — execute one tool once, deterministically.
///
/// Runs through the SAME executor a chat turn uses ([`ChatService::invoke_tool`]:
/// command-tool dispatch → Python/MCP tool server), minus the LLM. This is the
/// endpoint workflow `action: tool` steps call, and it returns the tool's real
/// result — no fake "accepted", no success audited for work that never ran
/// (the previous handler was an honest 501 that executed nothing; see
/// AUDIT_BACKLOG 0.2, now closed).
///
/// The tool runs as the authenticated caller (RBAC `ExecuteTools` is enforced
/// by the router layer). Unknown tools, and tools that fail, come back as
/// honest errors from the executor.
pub async fn run_tool(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Some(service) = state.chat.get().cloned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Tool executor is not running on this node — it needs a \
                        configured LLM ([chat] in ~/.prism/config.toml or [indexer] \
                        in prism.toml) and a Python tool environment. Check node logs."
                    .to_string(),
            }),
        )
            .into_response();
    };

    let platform_access = command_tool_platform_access(&state, &user).await;
    let body = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| Value::Object(Default::default()));
    // Approval-gated tools need the caller to say so explicitly — this is an
    // authenticated, RBAC-gated (ExecuteTools) local endpoint, so `"approve":
    // true` in the body stands in for the interactive approval of a chat turn.
    let approve = body
        .get("approve")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let args = build_tool_args(&body);

    match service
        .invoke_tool_with_actor_and_platform_access(
            &name,
            args,
            Some(&user.user_id),
            user.provenance_actor(),
            platform_access,
            approve,
        )
        .await
    {
        Ok(result) => Json(serde_json::json!({ "tool": name, "result": result })).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: format!("Tool '{name}' failed: {e:#}"),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::build_tool_args;
    use crate::NodeState;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;
    use tower::ServiceExt;

    const STUB_TOOL_SERVER: &str = r#"
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if request.get("method") == "list_tools":
        response = {"tools": [{
            "name": "local_only_test",
            "description": "A local-only test tool.",
            "input_schema": {"type": "object", "properties": {}},
            "requires_approval": False,
        }, {
            "name": "execute_python",
            "description": "Execute arbitrary Python (test only).",
            "input_schema": {"type": "object", "properties": {"code": {"type": "string"}}},
            "requires_approval": True,
        }]}
    elif request.get("method") == "call_tool":
        response = {"result": {"ok": True, "tool": request.get("tool")}}
    else:
        response = {"error": "unknown method"}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#;

    fn write_stub_project(dir: &Path) {
        let app = dir.join("app");
        std::fs::create_dir_all(&app).expect("create app directory");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER)
            .expect("write stub tool server");
    }

    async fn router_with_service(linked_platform: bool) -> Option<(axum::Router, String)> {
        let python = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| std::path::PathBuf::from("python3"))?;
        let project = tempfile::tempdir().expect("create tool project");
        write_stub_project(project.path());
        let tool_server = prism_python_bridge::ToolServer {
            python_bin: python,
            project_root: project.path().to_path_buf(),
            env: std::collections::BTreeMap::new(),
        };
        // Keep the project alive for the service child for the duration of the
        // test by moving it into a leaked test-owned allocation.
        let project = Box::leak(Box::new(project));
        let tool_server = prism_python_bridge::ToolServer {
            project_root: project.path().to_path_buf(),
            ..tool_server
        };
        let service = prism_agent::service::ChatService::spawn(
            prism_ingest::LlmConfig::default(),
            tool_server,
            Some(project.path().join("sessions")),
        )
        .await
        .expect("spawn chat service");

        let mut node = NodeState::new("test-node".into());
        if linked_platform {
            node.platform_client = Some(prism_client::PlatformClient::new("http://127.0.0.1:1"));
        }
        assert!(node.chat.set(Arc::new(service)).is_ok());
        let token = node.mint_offline_session_token();
        Some((crate::router::build_router(Arc::new(node)), token))
    }

    async fn router_with_authenticated_non_owner() -> Option<(axum::Router, String)> {
        let python = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| std::path::PathBuf::from("python3"))?;
        let project = Box::leak(Box::new(tempfile::tempdir().expect("create tool project")));
        write_stub_project(project.path());
        let service = prism_agent::service::ChatService::spawn(
            prism_ingest::LlmConfig::default(),
            prism_python_bridge::ToolServer {
                python_bin: python,
                project_root: project.path().to_path_buf(),
                env: std::collections::BTreeMap::new(),
            },
            Some(project.path().join("sessions")),
        )
        .await
        .expect("spawn chat service");

        let session_db = Box::leak(Box::new(
            tempfile::NamedTempFile::new().expect("create session database"),
        ));
        let manager = prism_core::session::SessionManager::new(
            session_db.path(),
            chrono::Duration::hours(24),
        )
        .expect("open session manager");
        let session = manager
            .create_session("authenticated:non-owner", None, None)
            .expect("create authenticated non-owner session");

        let mut node = NodeState::new("test-node".into());
        node.session_db_path = Some(session_db.path().to_path_buf());
        node.platform_client = Some(prism_client::PlatformClient::new("http://127.0.0.1:1"));
        node.platform_owner_id
            .set("owner-user".into())
            .expect("set platform owner");
        assert!(node.chat.set(Arc::new(service)).is_ok());
        Some((crate::router::build_router(Arc::new(node)), session.id))
    }

    async fn response_text(response: axum::response::Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        String::from_utf8(bytes.to_vec()).expect("response is utf8")
    }

    #[test]
    fn explicit_args_used_verbatim() {
        let body = json!({ "args": { "formula": "Fe2O3" } });
        assert_eq!(build_tool_args(&body), json!({ "formula": "Fe2O3" }));
    }

    #[test]
    fn workflow_inputs_and_command_folded() {
        let body =
            json!({ "command": "train", "inputs": { "data": "d.csv", "target": "hardness" } });
        assert_eq!(
            build_tool_args(&body),
            json!({ "data": "d.csv", "target": "hardness", "command": "train" })
        );
    }

    #[test]
    fn null_command_is_not_folded() {
        let body = json!({ "command": null, "inputs": { "x": 1 } });
        assert_eq!(build_tool_args(&body), json!({ "x": 1 }));
    }

    #[test]
    fn bare_object_is_the_args() {
        let body = json!({ "formula": "Si", "relax": true });
        assert_eq!(
            build_tool_args(&body),
            json!({ "formula": "Si", "relax": true })
        );
    }

    #[test]
    fn approve_flag_never_leaks_into_bare_args() {
        let body = json!({ "formula": "Si", "approve": true });
        assert_eq!(build_tool_args(&body), json!({ "formula": "Si" }));
    }

    #[test]
    fn caller_supplied_command_in_inputs_wins() {
        // If inputs already carries `command`, the envelope's command must not
        // clobber it (the tool author's explicit value takes precedence).
        let body = json!({ "command": "train", "inputs": { "command": "predict" } });
        assert_eq!(build_tool_args(&body), json!({ "command": "predict" }));
    }

    #[tokio::test]
    async fn post_tool_deploy_list_anonymous_capability_is_refused() {
        let Some((app, token)) = router_with_service(true).await else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/tools/deploy_list/run")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"approve":true}"#))
            .expect("build request");
        let response = app.oneshot(request).await.expect("run request");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response_text(response).await;
        assert!(body.contains("verified node-owner session"), "body: {body}");
        assert!(
            !body.contains("prism login"),
            "body must not suggest CLI login: {body}"
        );
        assert!(
            !body.contains("run `prism"),
            "body must not suggest a CLI command: {body}"
        );
    }

    #[tokio::test]
    async fn post_tool_deploy_create_approval_does_not_authorize_anonymous_caller() {
        let Some((app, token)) = router_with_service(true).await else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/tools/deploy_create/run")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"name":"test-deployment","image":"example/image","approve":true}"#,
            ))
            .expect("build request");
        let response = app.oneshot(request).await.expect("run request");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response_text(response).await;
        assert!(body.contains("verified node-owner session"), "body: {body}");
        assert!(
            !body.contains("prism login"),
            "body must not suggest CLI login: {body}"
        );
    }

    #[tokio::test]
    async fn non_owner_cannot_read_home_credentials_via_execute_python() {
        let Some((app, token)) = router_with_authenticated_non_owner().await else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/tools/execute_python/run")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "approve": true,
                    "code": "from pathlib import Path; print((Path.home() / '.prism' / 'credentials.json').read_text())"
                })
                .to_string(),
            ))
            .expect("build request");

        let response = app.oneshot(request).await.expect("run request");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response_text(response).await;
        assert!(body.contains("owner-only"), "body: {body}");
        assert!(
            body.contains("arbitrary code as the node OS user"),
            "body must state why environment filtering is insufficient: {body}"
        );
    }

    #[tokio::test]
    async fn anonymous_standalone_local_tool_still_succeeds() {
        let Some((app, token)) = router_with_service(false).await else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/tools/local_only_test/run")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("build request");
        let response = app.oneshot(request).await.expect("run request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        assert!(body.contains("local_only_test"), "body: {body}");
        assert!(body.contains(r#""ok":true"#), "body: {body}");
    }
}
