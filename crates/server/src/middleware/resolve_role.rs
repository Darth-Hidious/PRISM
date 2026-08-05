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
use prism_core::rbac::LocalRole;

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
/// 2. **Anonymous-local / offline** — the node has no verified account
///    identity, so grant a synthetic `NodeAdmin` role for local node
///    capabilities. This is deliberately not platform authority: handlers
///    that use the node owner's `PlatformClient` enforce a separate owner
///    check against the caller identity.
fn synthetic_local_role(user: &AuthenticatedUser) -> Option<LocalRole> {
    user.is_anonymous_local().then_some(LocalRole::NodeAdmin)
}

pub async fn resolve_role_layer(
    State(state): State<Arc<NodeState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(user) = req.extensions().get::<AuthenticatedUser>().cloned() {
        // Anonymous-local is allowed to administer this local node, but that
        // identity is never accepted by platform-owner handlers.
        if let Some(role) = synthetic_local_role(&user) {
            req.extensions_mut().insert(UserRole(role));
        } else {
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
                    // Offline / localhost-only mode preserves the existing
                    // local capability behavior for authenticated transport
                    // tokens. Platform handlers still reject this synthetic
                    // role unless the caller is independently verified.
                    req.extensions_mut().insert(UserRole(LocalRole::NodeAdmin));
                }
            }
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::AuthenticatedUser;
    use prism_core::rbac::Permission;

    #[test]
    fn anonymous_local_keeps_standalone_local_capabilities() {
        let user = AuthenticatedUser::anonymous_local();
        let role = synthetic_local_role(&user).expect("standalone caller must be anonymous-local");

        assert!(role.permissions().contains(&Permission::QueryData));
        assert!(role.permissions().contains(&Permission::ExecuteTools));
        assert!(role.permissions().contains(&Permission::IngestData));
    }
}
