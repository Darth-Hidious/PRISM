// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! External role adapters for hosted providers.
//!
//! PRISM owns the canonical [`LocalRole`] model. Provider role strings are
//! translated here, at the integration boundary, before they enter RBAC.

use anyhow::{Result, anyhow};
use prism_client::api::OrgMemberRole;
use prism_core::rbac::{ExternalRoleAssignment, LocalRole, RbacEngine};

/// Stable provenance key for assignments received from MARC27.
pub const MARC27_ROLE_PROVIDER: &str = "marc27";

/// Stable provenance key for assignments received from Supabase Auth.
pub const SUPABASE_ROLE_PROVIDER: &str = "supabase";

/// Provider-specific role adapter selected for a configured platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleProviderAdapter {
    Marc27,
    Supabase,
}

/// Select a role adapter only for a provider PRISM recognizes explicitly.
///
/// Unknown and absent providers fail closed: their role vocabulary is never
/// interpreted as MARC27's merely because the transport shape is compatible.
pub fn role_adapter_for(provider: Option<&str>) -> Option<RoleProviderAdapter> {
    match provider {
        Some(MARC27_ROLE_PROVIDER) => Some(RoleProviderAdapter::Marc27),
        Some(SUPABASE_ROLE_PROVIDER) => Some(RoleProviderAdapter::Supabase),
        _ => None,
    }
}

/// Outcome of reconciling a MARC27 membership response into PRISM RBAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marc27RoleSync {
    pub synced: usize,
    pub revoked: usize,
    pub ignored: usize,
}

/// Outcome of synchronizing the role carried by one verified Supabase login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupabaseRoleSync {
    /// Project-scoped canonical principal persisted in the PRISM session.
    pub principal_id: String,
    /// The recognized PRISM role, or `None` when the claim failed closed.
    pub role: Option<LocalRole>,
    /// Whether a stale assignment was removed for an unrecognized claim.
    pub revoked: bool,
}

/// Map a raw MARC27 organization role into PRISM's authorization model.
pub fn map_marc27_role(role: &str) -> Option<LocalRole> {
    match role {
        "owner" | "admin" => Some(LocalRole::NodeAdmin),
        "member" => Some(LocalRole::Engineer),
        "viewer" => Some(LocalRole::Viewer),
        _ => None,
    }
}

/// Map the Supabase Auth role claim into PRISM's authorization model.
///
/// Supabase's normal end-user role is deliberately read-only in PRISM.
/// Every other value fails closed, including the `anon` and `service_role`
/// API roles, which must never be interpreted as PRISM privileges.
pub fn map_supabase_role(role: &str) -> Option<LocalRole> {
    match role {
        "authenticated" => Some(LocalRole::Viewer),
        _ => None,
    }
}

/// Link a MARC27 subject to the canonical PRISM principal used by existing
/// verified sessions.
///
/// MARC27 account IDs already back PRISM's persisted session and RBAC IDs, so
/// Stage 1 deliberately preserves the string. Keeping this decision in the
/// adapter prevents future providers from gaining authority through an
/// accidental same-looking subject; they must explicitly namespace or link
/// their subjects to a PRISM principal.
pub fn map_marc27_principal(subject_id: &str) -> Option<String> {
    (!subject_id.trim().is_empty()).then(|| subject_id.to_string())
}

/// Link a Supabase subject to a project-scoped canonical PRISM principal.
///
/// Supabase subject identifiers are unique only within a project. The verified
/// canonical issuer is therefore hashed into the principal namespace. A
/// trailing slash is ignored so equivalent issuer spellings remain stable;
/// the fixed-size URL-safe digest also avoids ambiguous URL separators in the
/// persisted identifier.
pub fn map_supabase_principal(project_scope: &str, subject_id: &str) -> Option<String> {
    prism_client::auth::canonical_supabase_principal(project_scope, subject_id)
}

/// Synchronize a role claim from one signature-verified Supabase login.
///
/// Callers must pass the canonical issuer whose signature, issuer, audience,
/// and expiry were verified. Recognized claims replace any stale assignment;
/// unrecognized claims revoke the exact project-scoped subject so a previous
/// privilege cannot survive a downgrade.
pub fn sync_supabase_login_role(
    engine: &RbacEngine,
    project_scope: &str,
    subject_id: &str,
    role_claim: &str,
) -> Result<SupabaseRoleSync> {
    let principal_id = map_supabase_principal(project_scope, subject_id)
        .ok_or_else(|| anyhow!("Supabase issuer and subject must be non-empty canonical values"))?;
    let role = map_supabase_role(role_claim);
    let revoked = match role {
        Some(role) => {
            // The project-scoped principal is also the provider subject key;
            // raw Supabase subjects can repeat across distinct projects.
            engine.assign_external_role(
                SUPABASE_ROLE_PROVIDER,
                &principal_id,
                &principal_id,
                role,
            )?;
            false
        }
        None => engine.revoke_external_role(SUPABASE_ROLE_PROVIDER, &principal_id)?,
    };

    Ok(SupabaseRoleSync {
        principal_id,
        role,
        revoked,
    })
}

/// Reconcile MARC27 membership roles without touching PRISM-local roles or
/// assignments from any other provider.
pub fn reconcile_marc27_roles(
    engine: &RbacEngine,
    members: &[OrgMemberRole],
) -> Result<Marc27RoleSync> {
    let mut ignored = 0usize;
    let assignments = members
        .iter()
        .filter_map(|member| {
            let role = map_marc27_role(&member.role);
            let principal_id = map_marc27_principal(&member.user_id);
            match (role, principal_id) {
                (Some(role), Some(principal_id)) => Some(ExternalRoleAssignment::new(
                    member.user_id.clone(),
                    principal_id,
                    role,
                )),
                _ => {
                    ignored += 1;
                    None
                }
            }
        })
        .collect::<Vec<_>>();
    let result = engine.replace_external_roles(MARC27_ROLE_PROVIDER, &assignments)?;

    Ok(Marc27RoleSync {
        synced: result.assigned,
        revoked: result.removed,
        ignored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(user_id: &str, role: &str) -> OrgMemberRole {
        OrgMemberRole {
            user_id: user_id.to_string(),
            role: role.to_string(),
        }
    }

    #[test]
    fn marc27_adapter_has_an_exact_explicit_mapping() {
        assert_eq!(map_marc27_role("owner"), Some(LocalRole::NodeAdmin));
        assert_eq!(map_marc27_role("admin"), Some(LocalRole::NodeAdmin));
        assert_eq!(map_marc27_role("member"), Some(LocalRole::Engineer));
        assert_eq!(map_marc27_role("viewer"), Some(LocalRole::Viewer));
        assert_eq!(map_marc27_role("analyst"), None);
        assert_eq!(map_marc27_role("OWNER"), None);
        assert_eq!(map_marc27_role(""), None);
        assert_eq!(
            map_marc27_principal("user-123"),
            Some("user-123".to_string())
        );
        assert_eq!(map_marc27_principal(""), None);
        assert_eq!(map_marc27_principal("   "), None);
    }

    #[test]
    fn provider_dispatch_is_explicit_and_fails_closed() {
        assert_eq!(
            role_adapter_for(Some("marc27")),
            Some(RoleProviderAdapter::Marc27)
        );
        assert_eq!(
            role_adapter_for(Some("supabase")),
            Some(RoleProviderAdapter::Supabase)
        );
        assert_eq!(role_adapter_for(None), None);
        assert_eq!(role_adapter_for(Some("")), None);
        assert_eq!(role_adapter_for(Some("unknown")), None);
        assert_eq!(role_adapter_for(Some("MARC27")), None);
        assert_eq!(role_adapter_for(Some("Supabase")), None);
        assert_eq!(role_adapter_for(Some("https://api.marc27.com")), None);
    }

    #[test]
    fn supabase_adapter_maps_only_authenticated_to_viewer() {
        assert_eq!(map_supabase_role("authenticated"), Some(LocalRole::Viewer));
        assert_eq!(map_supabase_role("anon"), None);
        assert_eq!(map_supabase_role("service_role"), None);
        assert_eq!(map_supabase_role("admin"), None);
        assert_eq!(map_supabase_role("AUTHENTICATED"), None);
        assert_eq!(map_supabase_role(""), None);
    }

    #[test]
    fn supabase_principals_are_deterministic_and_project_scoped() {
        let first = map_supabase_principal("https://first.supabase.co/auth/v1", "user-123")
            .expect("valid principal");
        let equivalent = map_supabase_principal("https://first.supabase.co/auth/v1/", "user-123")
            .expect("valid principal");
        let another_project =
            map_supabase_principal("https://second.supabase.co/auth/v1", "user-123")
                .expect("valid principal");

        assert_eq!(first, equivalent);
        assert_ne!(first, another_project);
        assert!(first.starts_with("supabase:"));
        assert!(!first.contains("https://"));
        assert_eq!(map_supabase_principal("", "user-123"), None);
        assert_eq!(
            map_supabase_principal("https://first.supabase.co/auth/v1", " "),
            None
        );
    }

    #[test]
    fn supabase_login_downgrades_stale_privilege_to_viewer() {
        let engine = RbacEngine::in_memory().unwrap();
        let issuer = "https://project.supabase.co/auth/v1";
        let principal = map_supabase_principal(issuer, "user-123").unwrap();
        engine
            .assign_external_role(
                SUPABASE_ROLE_PROVIDER,
                &principal,
                &principal,
                LocalRole::NodeAdmin,
            )
            .unwrap();

        let result =
            sync_supabase_login_role(&engine, issuer, "user-123", "authenticated").unwrap();

        assert_eq!(result.principal_id, principal);
        assert_eq!(result.role, Some(LocalRole::Viewer));
        assert!(!result.revoked);
        assert_eq!(
            engine.get_role(&principal).unwrap(),
            Some(LocalRole::Viewer)
        );
        assert!(
            !engine
                .check_permission(&principal, prism_core::rbac::Permission::ManageNode)
                .unwrap()
        );
    }

    #[test]
    fn unrecognized_supabase_claim_revokes_stale_privilege_only_for_that_project() {
        let engine = RbacEngine::in_memory().unwrap();
        let first_issuer = "https://first.supabase.co/auth/v1";
        let second_issuer = "https://second.supabase.co/auth/v1";
        let first = map_supabase_principal(first_issuer, "shared-subject").unwrap();
        let second = map_supabase_principal(second_issuer, "shared-subject").unwrap();
        engine
            .assign_external_role(SUPABASE_ROLE_PROVIDER, &first, &first, LocalRole::NodeAdmin)
            .unwrap();
        engine
            .assign_external_role(
                SUPABASE_ROLE_PROVIDER,
                &second,
                &second,
                LocalRole::NodeAdmin,
            )
            .unwrap();

        let result =
            sync_supabase_login_role(&engine, first_issuer, "shared-subject", "service_role")
                .unwrap();

        assert_eq!(result.principal_id, first);
        assert_eq!(result.role, None);
        assert!(result.revoked);
        assert_eq!(engine.get_role(&first).unwrap(), None);
        assert!(
            !engine
                .check_permission(&first, prism_core::rbac::Permission::ManageNode)
                .unwrap()
        );
        assert_eq!(
            engine.get_role(&second).unwrap(),
            Some(LocalRole::NodeAdmin)
        );
    }

    #[test]
    fn marc27_reconciliation_is_provider_scoped() {
        let engine = RbacEngine::in_memory().unwrap();
        engine.assign_role("local", LocalRole::Engineer).unwrap();
        engine
            .assign_external_role(MARC27_ROLE_PROVIDER, "stale", "stale", LocalRole::Viewer)
            .unwrap();
        engine
            .assign_external_role(
                "another-provider",
                "stale",
                "another-stale",
                LocalRole::Analyst,
            )
            .unwrap();

        let result = reconcile_marc27_roles(
            &engine,
            &[
                member("current", "admin"),
                member("unknown", "unrecognized"),
            ],
        )
        .unwrap();

        assert_eq!(
            result,
            Marc27RoleSync {
                synced: 1,
                revoked: 1,
                ignored: 1,
            }
        );
        assert_eq!(
            engine.get_local_role("local").unwrap(),
            Some(LocalRole::Engineer)
        );
        assert_eq!(
            engine
                .get_external_role(MARC27_ROLE_PROVIDER, "stale")
                .unwrap(),
            None
        );
        assert_eq!(
            engine
                .get_external_role("another-provider", "stale")
                .unwrap(),
            Some(LocalRole::Analyst)
        );
        assert_eq!(
            engine
                .get_external_role(MARC27_ROLE_PROVIDER, "current")
                .unwrap(),
            Some(LocalRole::NodeAdmin)
        );
    }
}
