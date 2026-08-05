pub mod auth;
pub mod federation;
pub mod rbac;
pub mod resolve_role;

pub use auth::{
    ANONYMOUS_LOCAL_USER_ID, AuthenticatedUser, CallerIdentity, SessionToken,
    VERIFIED_SESSION_PREFIX, auth_layer,
};
pub use federation::federation_layer;
pub use rbac::{UserRole, require_permission};
pub use resolve_role::resolve_role_layer;
