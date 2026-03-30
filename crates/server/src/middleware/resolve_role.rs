//! Middleware that resolves an [`AuthenticatedUser`] into a [`UserRole`]
//! by looking up the local RBAC engine.
//!
//! Must run **after** [`auth_layer`] (which inserts `AuthenticatedUser`)
//! and **before** [`require_permission`] (which reads `UserRole`).

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use super::{AuthenticatedUser, UserRole};
use crate::NodeState;

/// Axum middleware that resolves the authenticated user's local role from
/// the RBAC SQLite database and inserts it into request extensions.
///
/// If the RBAC database is not configured or the user has no role, the
/// request proceeds without a `UserRole` extension — downstream
/// `require_permission` will return 403.
pub async fn resolve_role_layer(
    State(state): State<Arc<NodeState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(user) = req.extensions().get::<AuthenticatedUser>().cloned() {
        if let Some(ref db_path) = state.rbac_db_path {
            if let Ok(engine) = prism_core::rbac::RbacEngine::new(db_path) {
                if let Ok(Some(role)) = engine.get_role(&user.user_id) {
                    req.extensions_mut().insert(UserRole(role));
                }
            }
        }
    }
    next.run(req).await
}
