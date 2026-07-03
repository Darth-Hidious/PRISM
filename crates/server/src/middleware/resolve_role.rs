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
/// Two paths:
///
/// 1. **Online (configured RBAC DB)** — look up the user's role; if
///    found, insert `UserRole`. If not found, proceed without — the
///    downstream `require_permission` returns 403 with a clear
///    "no role assigned" message.
///
/// 2. **Offline / localhost-only (no RBAC DB)** — the daemon was
///    started with `--offline`, so the dashboard binds 127.0.0.1
///    only and there's no platform-issued identity. Grant the
///    authenticated user a synthetic `NodeAdmin` role. Without this,
///    Bug #21: `tests/test_mesh_e2e.sh` (and any local dev script
///    hitting the API) gets 403 because the user has no DB row, even
///    though the only thing on this socket is the developer's own
///    machine. Localhost+no-RBAC = trust-the-caller is the right
///    default; the auth layer's no-session-DB fallback already
///    accepts any non-empty token, so adding a default role here is
///    consistent with that posture.
pub async fn resolve_role_layer(
    State(state): State<Arc<NodeState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(user) = req.extensions().get::<AuthenticatedUser>().cloned() {
        match state.rbac_db_path.as_ref() {
            Some(db_path) => {
                if let Ok(engine) = prism_core::rbac::RbacEngine::new(db_path)
                    && let Ok(Some(role)) = engine.get_role(&user.user_id)
                {
                    req.extensions_mut().insert(UserRole(role));
                }
                // Else: leave UserRole absent → require_permission 403s
            }
            None => {
                // Offline / localhost-only mode: grant NodeAdmin so
                // local dev scripts can hit write endpoints. See
                // Bug #21 in docs/SHIPPED.md.
                req.extensions_mut()
                    .insert(UserRole(prism_core::rbac::LocalRole::NodeAdmin));
            }
        }
    }
    next.run(req).await
}
