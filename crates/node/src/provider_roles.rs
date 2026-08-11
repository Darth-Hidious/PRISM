// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! External role adapters for hosted providers.
//!
//! PRISM owns the canonical [`LocalRole`] model. Provider role strings are
//! translated here, at the integration boundary, before they enter RBAC.

use anyhow::Result;
use prism_client::api::OrgMemberRole;
use prism_core::rbac::{ExternalRoleAssignment, LocalRole, RbacEngine};

/// Stable provenance key for assignments received from MARC27.
pub const MARC27_ROLE_PROVIDER: &str = "marc27";

/// Provider-specific role adapter selected for a configured platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleProviderAdapter {
    Marc27,
}

/// Select a role adapter only for a provider PRISM recognizes explicitly.
///
/// Unknown and absent providers fail closed: their role vocabulary is never
/// interpreted as MARC27's merely because the transport shape is compatible.
pub fn role_adapter_for(provider: Option<&str>) -> Option<RoleProviderAdapter> {
    match provider {
        Some(MARC27_ROLE_PROVIDER) => Some(RoleProviderAdapter::Marc27),
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

/// Map a raw MARC27 organization role into PRISM's authorization model.
pub fn map_marc27_role(role: &str) -> Option<LocalRole> {
    match role {
        "owner" | "admin" => Some(LocalRole::NodeAdmin),
        "member" => Some(LocalRole::Engineer),
        "viewer" => Some(LocalRole::Viewer),
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
        assert_eq!(role_adapter_for(None), None);
        assert_eq!(role_adapter_for(Some("")), None);
        assert_eq!(role_adapter_for(Some("unknown")), None);
        assert_eq!(role_adapter_for(Some("MARC27")), None);
        assert_eq!(role_adapter_for(Some("https://api.marc27.com")), None);
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
