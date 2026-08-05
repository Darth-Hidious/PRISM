//! Deployment endpoints — the platform's deployment surface, proxied through
//! the node's authenticated `PlatformClient`.
//!
//! The chat app gets first-class deployment control (list / create / status /
//! stop) without re-implementing platform auth. All tenancy, solvency, and
//! billing enforcement stays platform-side — this is a pass-through, not a
//! second implementation.

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::NodeState;
use crate::middleware::AuthenticatedUser;
use prism_agent::command_tools::CommandToolPlatformAccess;

type HandlerError = (StatusCode, Json<Value>);

fn error(status: StatusCode, message: impl Into<String>) -> HandlerError {
    (status, Json(json!({ "error": message.into() })))
}

fn no_platform() -> HandlerError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "No linked platform session is available on this node.",
    )
}

fn platform_denied(message: impl Into<String>) -> HandlerError {
    error(StatusCode::FORBIDDEN, message)
}

fn platform_verification_failed() -> HandlerError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Unable to verify the node owner with the linked platform session; \
         re-authentication is required before accessing platform resources.",
    )
}

/// Return the platform client only after the request identity is established
/// and matches the identity carried by the node's stored platform credential.
/// Anonymous-local callers retain local node capabilities but can never become
/// the node owner merely by reaching loopback or choosing a token/user_id.
async fn authorized_platform_client<'a>(
    state: &'a NodeState,
    caller: &AuthenticatedUser,
) -> Result<&'a prism_client::PlatformClient, HandlerError> {
    if !caller.is_authenticated() {
        return Err(platform_denied(
            "Platform access denied: this is an anonymous-local caller. Authenticate as the node owner with a verified platform session; loopback reachability and user_id hints do not grant owner access.",
        ));
    }

    let Some(client) = state.platform_client.as_ref() else {
        return Err(no_platform());
    };

    let owner_id = if let Some(owner_id) = state.platform_owner_id.get() {
        owner_id.clone()
    } else {
        let owner = client
            .fetch_current_user()
            .await
            .map_err(|_| platform_verification_failed())?;
        let owner_id = owner.id;
        let _ = state.platform_owner_id.set(owner_id.clone());
        owner_id
    };

    if owner_id != caller.user_id {
        return Err(platform_denied(
            "Platform access denied: the authenticated session is not the identity linked to this node. Authenticate as the node owner before accessing deployments or spending owner credits.",
        ));
    }

    Ok(client)
}

/// Resolve the credential boundary for agent/HTTP tool execution. This is
/// intentionally capability-based rather than a list of platform tool names:
/// every CLI child gets the same result, including tools added later.
pub(crate) async fn command_tool_platform_access(
    state: &NodeState,
    caller: &AuthenticatedUser,
) -> CommandToolPlatformAccess {
    if state.platform_client.is_none() {
        return CommandToolPlatformAccess::LocalOnly;
    }
    if authorized_platform_client(state, caller).await.is_ok() {
        CommandToolPlatformAccess::VerifiedNodeOwner
    } else {
        CommandToolPlatformAccess::UnverifiedHttp
    }
}

fn upstream(e: anyhow::Error) -> HandlerError {
    error(
        StatusCode::BAD_GATEWAY,
        format!("platform request failed: {e}"),
    )
}

/// GET /api/deployments — list deployments visible to the node owner.
pub async fn list_deployments(
    State(state): State<Arc<NodeState>>,
    Extension(caller): Extension<AuthenticatedUser>,
) -> Result<Json<Value>, HandlerError> {
    let client = authorized_platform_client(&state, &caller).await?;
    let deployments: Value = client.get("/compute/deployments").await.map_err(upstream)?;
    Ok(Json(deployments))
}

/// POST /api/deployments — create a deployment (body forwarded verbatim).
pub async fn create_deployment(
    State(state): State<Arc<NodeState>>,
    Extension(caller): Extension<AuthenticatedUser>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, HandlerError> {
    let client = authorized_platform_client(&state, &caller).await?;
    let created: Value = client
        .post("/compute/deployments", &body)
        .await
        .map_err(upstream)?;
    Ok(Json(created))
}

/// GET /api/deployments/{id} — one deployment's status.
pub async fn get_deployment(
    State(state): State<Arc<NodeState>>,
    Extension(caller): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HandlerError> {
    let client = authorized_platform_client(&state, &caller).await?;
    let deployment: Value = client
        .get(&format!("/compute/deployments/{id}"))
        .await
        .map_err(upstream)?;
    Ok(Json(deployment))
}

/// DELETE /api/deployments/{id} — stop a deployment.
pub async fn stop_deployment(
    State(state): State<Arc<NodeState>>,
    Extension(caller): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HandlerError> {
    let client = authorized_platform_client(&state, &caller).await?;
    client
        .delete(&format!("/compute/deployments/{id}"))
        .await
        .map_err(upstream)?;
    Ok(Json(json!({ "stopped": id })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::{AuthenticatedUser, VERIFIED_SESSION_PREFIX};

    #[tokio::test]
    async fn anonymous_local_cannot_reach_platform_client() {
        let mut node = NodeState::new("test-node".into());
        node.platform_client = Some(prism_client::PlatformClient::new("http://127.0.0.1:1"));
        let state = Arc::new(node);

        let result = list_deployments(
            State(state),
            Extension(AuthenticatedUser::anonymous_local()),
        )
        .await;

        match result {
            Err((status, Json(body))) => {
                assert_eq!(status, StatusCode::FORBIDDEN);
                assert!(body["error"].as_str().unwrap().contains("anonymous-local"));
            }
            Ok(_) => panic!("anonymous-local caller reached the platform path"),
        }
    }

    #[tokio::test]
    async fn authenticated_non_owner_is_refused_before_deployment_request() {
        let mut node = NodeState::new("test-node".into());
        node.platform_client = Some(prism_client::PlatformClient::new("http://127.0.0.1:1"));
        node.platform_owner_id.set("owner-123".into()).unwrap();
        let state = Arc::new(node);
        let caller =
            AuthenticatedUser::from_session_user_id(format!("{VERIFIED_SESSION_PREFIX}other-user"));

        let result = list_deployments(State(state), Extension(caller)).await;

        match result {
            Err((status, Json(body))) => {
                assert_eq!(status, StatusCode::FORBIDDEN);
                assert!(body["error"].as_str().unwrap().contains("not the identity"));
            }
            Ok(_) => panic!("non-owner caller reached the deployment path"),
        }
    }
}
