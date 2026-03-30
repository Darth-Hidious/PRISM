//! Role-based access control middleware for the PRISM node HTTP API.
//!
//! Provides [`require_permission`], which returns an Axum middleware that
//! checks whether the authenticated user has the required [`Permission`].
//!
//! The user's role is expected to be in request extensions as [`UserRole`],
//! inserted by an earlier layer (e.g. after session validation resolves
//! the user against the RBAC engine). If missing, 403 is returned.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use prism_core::rbac::{LocalRole, Permission};
use serde::Serialize;

/// Inserted into request extensions by a layer that resolves the user's local role.
/// For now, handlers or test harnesses can insert this manually; once the RBAC
/// engine is wired into shared state this will happen automatically.
#[derive(Debug, Clone)]
pub struct UserRole(pub LocalRole);

#[derive(Serialize)]
struct ForbiddenBody {
    error: &'static str,
    message: String,
}

/// Returns an Axum middleware function that enforces a required [`Permission`].
///
/// Usage in a router:
/// ```ignore
/// use axum::middleware;
/// use prism_core::rbac::Permission;
///
/// Router::new()
///     .route("/admin", get(admin_handler))
///     .layer(middleware::from_fn(require_permission(Permission::ManageNode)))
/// ```
pub fn require_permission(
    required: Permission,
) -> impl Fn(Request, Next) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
       + Clone
       + Send {
    move |req: Request, next: Next| {
        let required = required;
        Box::pin(async move {
            let role = req.extensions().get::<UserRole>().cloned();

            match role {
                Some(UserRole(local_role)) if local_role.permissions().contains(&required) => {
                    next.run(req).await
                }
                Some(UserRole(local_role)) => {
                    let body = ForbiddenBody {
                        error: "forbidden",
                        message: format!(
                            "Role {:?} does not have permission {:?}",
                            local_role, required
                        ),
                    };
                    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
                }
                None => {
                    let body = ForbiddenBody {
                        error: "forbidden",
                        message: "No role assigned. Contact the node administrator.".into(),
                    };
                    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
                }
            }
        })
    }
}
