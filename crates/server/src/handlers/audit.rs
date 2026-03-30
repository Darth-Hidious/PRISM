//! Audit log handlers.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::NodeState;

#[derive(Serialize)]
pub struct AuditEntryResponse {
    pub id: i64,
    pub timestamp: String,
    pub user_id: String,
    pub action: String,
    pub target: String,
    pub detail: Option<String>,
    pub outcome: String,
}

#[derive(Deserialize)]
pub struct AuditQueryParams {
    pub user_id: Option<String>,
    pub action: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// GET /api/audit — list recent audit log entries with optional filters.
pub async fn list_audit_log(
    State(state): State<Arc<NodeState>>,
    Query(params): Query<AuditQueryParams>,
) -> Result<Json<Vec<AuditEntryResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let Some(ref db_path) = state.audit_db_path else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Audit log not configured — node not fully initialized.".into(),
            }),
        ));
    };

    let log = prism_core::audit::AuditLog::new(db_path).map_err(|e| {
        tracing::error!(error = %e, "failed to open audit database");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error.".into(),
            }),
        )
    })?;

    let filter = prism_core::audit::AuditFilter {
        user_id: params.user_id,
        limit: params.limit,
        ..Default::default()
    };

    let entries = log.query(&filter).map_err(|e| {
        tracing::error!(error = %e, "failed to query audit log");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Internal server error.".into(),
            }),
        )
    })?;

    let result = entries
        .into_iter()
        .map(|e| AuditEntryResponse {
            id: e.id,
            timestamp: e.timestamp.to_rfc3339(),
            user_id: e.user_id,
            action: format!("{}", e.action),
            target: e.target,
            detail: e.detail,
            outcome: format!("{}", e.outcome),
        })
        .collect();

    Ok(Json(result))
}
