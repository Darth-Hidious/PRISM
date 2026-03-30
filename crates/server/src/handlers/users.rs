//! User management handlers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::middleware::UserRole;
use crate::NodeState;

#[derive(Serialize)]
pub struct UserInfo {
    pub id: String,
    pub role: String,
    pub permissions: Vec<String>,
}

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub user_id: String,
    pub role: Option<String>,
}

#[derive(Serialize)]
pub struct CreateUserResponse {
    pub status: &'static str,
    pub message: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// GET /api/users — list all users with local roles.
pub async fn list_users(
    State(state): State<Arc<NodeState>>,
) -> Result<Json<Vec<UserInfo>>, (StatusCode, Json<ErrorResponse>)> {
    let Some(ref db_path) = state.rbac_db_path else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "RBAC not configured — node not fully initialized.".into(),
            }),
        ));
    };

    let engine = prism_core::rbac::RbacEngine::new(db_path).map_err(|e| {
        tracing::error!(error = %e, "failed to open RBAC database");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error.".into(),
            }),
        )
    })?;

    let users = engine.list_users().map_err(|e| {
        tracing::error!(error = %e, "failed to list users");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error.".into(),
            }),
        )
    })?;

    let result = users
        .into_iter()
        .map(|(uid, role)| UserInfo {
            id: uid,
            permissions: role
                .permissions()
                .iter()
                .map(|p| format!("{p:?}"))
                .collect(),
            role: format!("{role:?}"),
        })
        .collect();

    Ok(Json(result))
}

/// POST /api/users — create or update a user's role.
pub async fn create_user(
    State(state): State<Arc<NodeState>>,
    caller_role: Option<Extension<UserRole>>,
    Json(body): Json<CreateUserRequest>,
) -> Result<Json<CreateUserResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Input validation
    if body.user_id.is_empty() || body.user_id.len() > 256 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "user_id must be 1-256 characters.".into(),
            }),
        ));
    }
    if !body.user_id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '@') {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "user_id contains invalid characters.".into(),
            }),
        ));
    }

    let Some(ref db_path) = state.rbac_db_path else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "RBAC not configured — node not fully initialized.".into(),
            }),
        ));
    };

    let role_str = body.role.as_deref().unwrap_or("viewer");
    let role = match role_str {
        "node_admin" => prism_core::rbac::LocalRole::NodeAdmin,
        "engineer" => prism_core::rbac::LocalRole::Engineer,
        "analyst" => prism_core::rbac::LocalRole::Analyst,
        "viewer" => prism_core::rbac::LocalRole::Viewer,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!(
                        "Unknown role: '{other}'. Use: node_admin, engineer, analyst, viewer."
                    ),
                }),
            ));
        }
    };

    // Privilege boundary: only NodeAdmin can assign NodeAdmin
    if role == prism_core::rbac::LocalRole::NodeAdmin {
        let is_admin = caller_role
            .as_ref()
            .map(|Extension(r)| r.0 == prism_core::rbac::LocalRole::NodeAdmin)
            .unwrap_or(false);
        if !is_admin {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "Only NodeAdmin can assign the node_admin role.".into(),
                }),
            ));
        }
    }

    let engine = prism_core::rbac::RbacEngine::new(db_path).map_err(|e| {
        tracing::error!(error = %e, "failed to open RBAC database");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error.".into(),
            }),
        )
    })?;

    engine
        .assign_role(&body.user_id, role)
        .map_err(|e| {
            tracing::error!(error = %e, user_id = %body.user_id, "failed to assign role");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Internal server error.".into(),
                }),
            )
        })?;

    Ok(Json(CreateUserResponse {
        status: "created",
        message: format!("User '{}' assigned role '{role_str}'.", body.user_id),
    }))
}
