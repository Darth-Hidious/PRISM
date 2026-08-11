//! Role-Based Access Control engine for PRISM nodes.
//!
//! [`LocalRole`] is PRISM's canonical authorization model. Roles obtained from
//! hosted services are mapped into this model by provider adapters and stored
//! separately from roles assigned by a PRISM node administrator.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// ---------------------------------------------------------------------------
// PRISM roles
// ---------------------------------------------------------------------------

/// A role defined and enforced by PRISM.
///
/// Provider-specific role names must be mapped into this enum at the provider
/// boundary. They are not part of PRISM's authorization model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRole {
    NodeAdmin,
    Engineer,
    Analyst,
    Viewer,
}

impl LocalRole {
    /// Returns the set of permissions granted to this role.
    pub fn permissions(&self) -> &[Permission] {
        match self {
            LocalRole::NodeAdmin => &[
                Permission::ManageNode,
                Permission::ManageUsers,
                Permission::ExecuteTools,
                Permission::PublishData,
                Permission::IngestData,
                Permission::QueryData,
                Permission::ViewDashboard,
                Permission::ViewAudit,
            ],
            LocalRole::Engineer => &[
                Permission::ExecuteTools,
                Permission::PublishData,
                Permission::IngestData,
                Permission::QueryData,
                Permission::ViewDashboard,
            ],
            LocalRole::Analyst => &[
                Permission::QueryData,
                Permission::ViewDashboard,
                Permission::ViewAudit,
            ],
            LocalRole::Viewer => &[Permission::ViewDashboard],
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            LocalRole::NodeAdmin => "node_admin",
            LocalRole::Engineer => "engineer",
            LocalRole::Analyst => "analyst",
            LocalRole::Viewer => "viewer",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "node_admin" => Some(LocalRole::NodeAdmin),
            "engineer" => Some(LocalRole::Engineer),
            "analyst" => Some(LocalRole::Analyst),
            "viewer" => Some(LocalRole::Viewer),
            _ => None,
        }
    }

    /// PRISM's explicit conflict policy for multiple provider assignments that
    /// an adapter linked to one canonical principal.
    ///
    /// This tier selects one effective role; it is not inferred from permission
    /// set inclusion. Locally assigned roles always take precedence.
    fn effective_role_priority(self) -> u8 {
        match self {
            LocalRole::NodeAdmin => 3,
            LocalRole::Engineer => 2,
            LocalRole::Analyst => 1,
            LocalRole::Viewer => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ManageNode,
    ManageUsers,
    ExecuteTools,
    PublishData,
    IngestData,
    QueryData,
    ViewDashboard,
    ViewAudit,
}

/// Result of atomically replacing one provider's external role assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalRoleReconciliation {
    pub assigned: usize,
    pub removed: usize,
}

/// A provider subject explicitly linked to a canonical PRISM principal.
///
/// Provider adapters own this identity mapping. Equal subject strings from
/// different providers do not collide unless their adapters intentionally link
/// them to the same `principal_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRoleAssignment {
    pub subject_id: String,
    pub principal_id: String,
    pub role: LocalRole,
}

impl ExternalRoleAssignment {
    pub fn new(
        subject_id: impl Into<String>,
        principal_id: impl Into<String>,
        role: LocalRole,
    ) -> Self {
        Self {
            subject_id: subject_id.into(),
            principal_id: principal_id.into(),
            role,
        }
    }
}

// ---------------------------------------------------------------------------
// RBAC Engine
// ---------------------------------------------------------------------------

/// SQLite-backed engine for PRISM-local and provider-scoped role assignments.
pub struct RbacEngine {
    conn: Connection,
}

impl RbacEngine {
    /// Open (or create) an RBAC database at the given path.
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open RBAC database at {}", db_path.display()))?;
        let engine = Self { conn };
        engine.init_schema()?;
        Ok(engine)
    }

    /// Create an in-memory RBAC engine (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().context("failed to open in-memory RBAC database")?;
        let engine = Self { conn };
        engine.init_schema()?;
        Ok(engine)
    }

    fn init_schema(&self) -> Result<()> {
        let had_local_role_table = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'user_roles'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let had_external_role_table = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'external_role_assignments'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let grandfathered_roles = if had_local_role_table && !had_external_role_table {
            self.conn
                .query_row("SELECT COUNT(*) FROM user_roles", [], |row| {
                    row.get::<_, i64>(0)
                })?
        } else {
            0
        };

        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS user_roles (
                    user_id  TEXT PRIMARY KEY NOT NULL,
                    role     TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS external_role_assignments (
                    provider    TEXT NOT NULL,
                    subject_id  TEXT NOT NULL,
                    principal_id TEXT NOT NULL,
                    role        TEXT NOT NULL,
                    PRIMARY KEY (provider, subject_id)
                );

                CREATE INDEX IF NOT EXISTS idx_external_role_assignments_principal
                    ON external_role_assignments (principal_id);",
            )
            .context("failed to initialize RBAC schema")?;

        // Before provider provenance was recorded, this table held a mixture
        // of administrator-created and provider-synced roles. Inferring which
        // is which could delete a genuine local grant, so every ambiguous row
        // is deliberately grandfathered as PRISM-local. The new external
        // table makes all subsequent provider assignments revocable by source.
        if grandfathered_roles > 0 {
            tracing::warn!(
                count = grandfathered_roles,
                "pre-separation RBAC assignments have unknown provenance; treating them as PRISM-local; review local user roles"
            );
        }
        Ok(())
    }

    /// Assign (or update) a local role for a user.
    pub fn assign_role(&self, user_id: &str, role: LocalRole) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO user_roles (user_id, role) VALUES (?1, ?2)
                 ON CONFLICT(user_id) DO UPDATE SET role = excluded.role",
                params![user_id, role.as_str()],
            )
            .with_context(|| format!("failed to assign role for user {user_id}"))?;
        tracing::info!(user_id, role = role.as_str(), "role assigned");
        Ok(())
    }

    /// Look up a role assigned directly by a PRISM node administrator.
    pub fn get_local_role(&self, user_id: &str) -> Result<Option<LocalRole>> {
        let mut stmt = self
            .conn
            .prepare("SELECT role FROM user_roles WHERE user_id = ?1")?;
        let role = stmt
            .query_row(params![user_id], |row| row.get::<_, String>(0))
            .optional()?
            .and_then(|s| LocalRole::from_str(&s));
        Ok(role)
    }

    /// Look up the effective PRISM role for a user.
    ///
    /// A local assignment is authoritative. If no local role exists, the
    /// highest-priority provider role explicitly linked to this canonical
    /// PRISM principal is used.
    pub fn get_role(&self, principal_id: &str) -> Result<Option<LocalRole>> {
        if let Some(role) = self.get_local_role(principal_id)? {
            return Ok(Some(role));
        }

        let mut stmt = self
            .conn
            .prepare("SELECT role FROM external_role_assignments WHERE principal_id = ?1")?;
        let rows = stmt
            .query_map(params![principal_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows
            .into_iter()
            .filter_map(|role| LocalRole::from_str(&role))
            .max_by_key(|role| role.effective_role_priority()))
    }

    /// Remove a user's local role assignment.
    pub fn remove_role(&self, user_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM user_roles WHERE user_id = ?1",
            params![user_id],
        )?;
        tracing::info!(user_id, "role removed");
        Ok(())
    }

    /// List roles assigned directly by a PRISM node administrator.
    pub fn list_local_users(&self) -> Result<Vec<(String, LocalRole)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT user_id, role FROM user_roles ORDER BY user_id")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows
            .into_iter()
            .filter_map(|(uid, r)| LocalRole::from_str(&r).map(|role| (uid, role)))
            .collect())
    }

    /// List all users and their effective PRISM roles.
    pub fn list_users(&self) -> Result<Vec<(String, LocalRole)>> {
        let mut users = BTreeMap::<String, LocalRole>::new();
        let mut stmt = self.conn.prepare(
            "SELECT principal_id, role FROM external_role_assignments
             ORDER BY principal_id, provider, subject_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for (user_id, raw_role) in rows {
            let Some(role) = LocalRole::from_str(&raw_role) else {
                continue;
            };
            users
                .entry(user_id)
                .and_modify(|current| {
                    if role.effective_role_priority() > current.effective_role_priority() {
                        *current = role;
                    }
                })
                .or_insert(role);
        }

        // Local assignments are authoritative and replace any external result.
        for (user_id, role) in self.list_local_users()? {
            users.insert(user_id, role);
        }

        Ok(users.into_iter().collect())
    }

    /// Assign or update a role mapped by an external identity provider.
    pub fn assign_external_role(
        &self,
        provider: &str,
        subject_id: &str,
        principal_id: &str,
        role: LocalRole,
    ) -> Result<()> {
        ensure!(
            !provider.trim().is_empty(),
            "external role provider must not be empty"
        );
        ensure!(
            !subject_id.trim().is_empty(),
            "external role subject must not be empty"
        );
        ensure!(
            !principal_id.trim().is_empty(),
            "external role principal must not be empty"
        );
        self.conn
            .execute(
                "INSERT INTO external_role_assignments
                    (provider, subject_id, principal_id, role)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider, subject_id) DO UPDATE SET
                    principal_id = excluded.principal_id,
                    role = excluded.role",
                params![provider, subject_id, principal_id, role.as_str()],
            )
            .with_context(|| {
                format!("failed to assign external role for subject {subject_id} from {provider}")
            })?;
        tracing::info!(
            provider,
            subject_id,
            principal_id,
            role = role.as_str(),
            "external role assigned"
        );
        Ok(())
    }

    /// Look up one provider's mapped PRISM role for a user.
    pub fn get_external_role(&self, provider: &str, subject_id: &str) -> Result<Option<LocalRole>> {
        let mut stmt = self.conn.prepare(
            "SELECT role FROM external_role_assignments
             WHERE provider = ?1 AND subject_id = ?2",
        )?;
        let role = stmt
            .query_row(params![provider, subject_id], |row| row.get::<_, String>(0))
            .optional()?
            .and_then(|role| LocalRole::from_str(&role));
        Ok(role)
    }

    /// Revoke one external assignment identified by its provider and subject.
    ///
    /// The compound key is intentional: revoking a Supabase subject, for
    /// example, can never remove a same-looking subject owned by another
    /// provider or a role assigned directly by a PRISM administrator.
    /// Returns `true` when an assignment existed and was removed.
    pub fn revoke_external_role(&self, provider: &str, subject_id: &str) -> Result<bool> {
        ensure!(
            !provider.trim().is_empty(),
            "external role provider must not be empty"
        );
        ensure!(
            !subject_id.trim().is_empty(),
            "external role subject must not be empty"
        );

        let removed = self
            .conn
            .execute(
                "DELETE FROM external_role_assignments
                 WHERE provider = ?1 AND subject_id = ?2",
                params![provider, subject_id],
            )
            .with_context(|| {
                format!("failed to revoke external role for subject {subject_id} from {provider}")
            })?;
        if removed > 0 {
            tracing::info!(provider, subject_id, "external role revoked");
        }
        Ok(removed > 0)
    }

    /// List one provider's mapped PRISM role assignments.
    pub fn list_external_roles(&self, provider: &str) -> Result<Vec<ExternalRoleAssignment>> {
        let mut stmt = self.conn.prepare(
            "SELECT subject_id, principal_id, role FROM external_role_assignments
             WHERE provider = ?1 ORDER BY subject_id",
        )?;
        let rows = stmt
            .query_map(params![provider], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows
            .into_iter()
            .filter_map(|(subject_id, principal_id, role)| {
                LocalRole::from_str(&role).map(|role| ExternalRoleAssignment {
                    subject_id,
                    principal_id,
                    role,
                })
            })
            .collect())
    }

    /// Atomically replace all mapped roles from one external provider.
    ///
    /// Assignments from other providers and PRISM-local assignments are never
    /// modified. Duplicate subjects in `assignments` resolve to the last link.
    pub fn replace_external_roles(
        &self,
        provider: &str,
        assignments: &[ExternalRoleAssignment],
    ) -> Result<ExternalRoleReconciliation> {
        ensure!(
            !provider.trim().is_empty(),
            "external role provider must not be empty"
        );

        for assignment in assignments {
            ensure!(
                !assignment.subject_id.trim().is_empty(),
                "external role subject must not be empty"
            );
            ensure!(
                !assignment.principal_id.trim().is_empty(),
                "external role principal must not be empty"
            );
        }
        let desired = assignments
            .iter()
            .map(|assignment| (assignment.subject_id.as_str(), assignment))
            .collect::<BTreeMap<_, _>>();
        let desired_ids = desired
            .keys()
            .map(|subject_id| (*subject_id).to_string())
            .collect::<BTreeSet<_>>();
        let existing_ids = self
            .list_external_roles(provider)?
            .into_iter()
            .map(|assignment| assignment.subject_id)
            .collect::<BTreeSet<_>>();

        let transaction = self
            .conn
            .unchecked_transaction()
            .context("failed to start external role reconciliation")?;
        transaction.execute(
            "DELETE FROM external_role_assignments WHERE provider = ?1",
            params![provider],
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO external_role_assignments
                    (provider, subject_id, principal_id, role)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (subject_id, assignment) in &desired {
                insert.execute(params![
                    provider,
                    subject_id,
                    assignment.principal_id,
                    assignment.role.as_str()
                ])?;
            }
        }
        transaction
            .commit()
            .context("failed to commit external role reconciliation")?;

        Ok(ExternalRoleReconciliation {
            assigned: desired.len(),
            removed: existing_ids.difference(&desired_ids).count(),
        })
    }

    /// Check whether a user has a specific permission.
    ///
    /// Returns `false` if the user has no role assigned.
    pub fn check_permission(&self, user_id: &str, permission: Permission) -> Result<bool> {
        match self.get_role(user_id)? {
            Some(role) => Ok(role.permissions().contains(&permission)),
            None => Ok(false),
        }
    }
}

// We need the `optional` helper from rusqlite.
use rusqlite::OptionalExtension;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> RbacEngine {
        RbacEngine::in_memory().expect("in-memory engine")
    }

    fn external(subject_id: &str, principal_id: &str, role: LocalRole) -> ExternalRoleAssignment {
        ExternalRoleAssignment::new(subject_id, principal_id, role)
    }

    #[test]
    fn assign_and_retrieve_role() {
        let e = engine();
        e.assign_role("alice", LocalRole::Engineer).unwrap();
        assert_eq!(e.get_role("alice").unwrap(), Some(LocalRole::Engineer));
    }

    #[test]
    fn reassign_overwrites_role() {
        let e = engine();
        e.assign_role("bob", LocalRole::Viewer).unwrap();
        e.assign_role("bob", LocalRole::Analyst).unwrap();
        assert_eq!(e.get_role("bob").unwrap(), Some(LocalRole::Analyst));
    }

    #[test]
    fn unknown_user_returns_none() {
        let e = engine();
        assert_eq!(e.get_role("ghost").unwrap(), None);
    }

    #[test]
    fn remove_role() {
        let e = engine();
        e.assign_role("carol", LocalRole::NodeAdmin).unwrap();
        e.remove_role("carol").unwrap();
        assert_eq!(e.get_role("carol").unwrap(), None);
    }

    #[test]
    fn list_users() {
        let e = engine();
        e.assign_role("alice", LocalRole::Engineer).unwrap();
        e.assign_role("bob", LocalRole::Viewer).unwrap();
        let users = e.list_users().unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.contains(&("alice".into(), LocalRole::Engineer)));
        assert!(users.contains(&("bob".into(), LocalRole::Viewer)));
    }

    #[test]
    fn local_assignment_is_authoritative_over_external_role() {
        let e = engine();
        e.assign_external_role(
            "provider-a",
            "external-alice",
            "alice",
            LocalRole::NodeAdmin,
        )
        .unwrap();
        assert_eq!(e.get_role("alice").unwrap(), Some(LocalRole::NodeAdmin));

        e.assign_role("alice", LocalRole::Viewer).unwrap();

        assert_eq!(e.get_role("alice").unwrap(), Some(LocalRole::Viewer));
        assert!(!e.check_permission("alice", Permission::ManageNode).unwrap());
    }

    #[test]
    fn local_assignment_survives_provider_revocation() {
        let e = engine();
        e.assign_role("alice", LocalRole::Engineer).unwrap();
        e.assign_external_role(
            "provider-a",
            "external-alice",
            "alice",
            LocalRole::NodeAdmin,
        )
        .unwrap();

        let result = e.replace_external_roles("provider-a", &[]).unwrap();

        assert_eq!(result.removed, 1);
        assert_eq!(
            e.get_local_role("alice").unwrap(),
            Some(LocalRole::Engineer)
        );
        assert_eq!(e.get_role("alice").unwrap(), Some(LocalRole::Engineer));
        assert_eq!(
            e.get_external_role("provider-a", "external-alice").unwrap(),
            None
        );
    }

    #[test]
    fn provider_reconciliation_removes_only_its_stale_assignments() {
        let e = engine();
        e.assign_external_role("provider-a", "stale", "stale", LocalRole::Viewer)
            .unwrap();
        e.assign_external_role("provider-a", "current", "current", LocalRole::Viewer)
            .unwrap();
        e.assign_external_role(
            "another-provider",
            "stale",
            "another-stale",
            LocalRole::Analyst,
        )
        .unwrap();

        let result = e
            .replace_external_roles(
                "provider-a",
                &[external("current", "current", LocalRole::Engineer)],
            )
            .unwrap();

        assert_eq!(
            result,
            ExternalRoleReconciliation {
                assigned: 1,
                removed: 1,
            }
        );
        assert_eq!(e.get_external_role("provider-a", "stale").unwrap(), None);
        assert_eq!(
            e.get_external_role("provider-a", "current").unwrap(),
            Some(LocalRole::Engineer)
        );
        assert_eq!(
            e.get_external_role("another-provider", "stale").unwrap(),
            Some(LocalRole::Analyst)
        );
    }

    #[test]
    fn highest_external_role_is_effective_without_a_local_assignment() {
        let e = engine();
        e.assign_external_role(
            "provider-a",
            "provider-a-alice",
            "alice",
            LocalRole::Analyst,
        )
        .unwrap();
        e.assign_external_role(
            "provider-b",
            "provider-b-alice",
            "alice",
            LocalRole::Engineer,
        )
        .unwrap();

        assert_eq!(e.get_role("alice").unwrap(), Some(LocalRole::Engineer));
        assert!(
            e.check_permission("alice", Permission::ExecuteTools)
                .unwrap()
        );
        assert!(!e.check_permission("alice", Permission::ViewAudit).unwrap());
    }

    #[test]
    fn equal_provider_subjects_do_not_collide_without_an_explicit_principal_link() {
        let e = engine();
        e.assign_external_role(
            "provider-a",
            "shared-subject",
            "prism-alice",
            LocalRole::Engineer,
        )
        .unwrap();
        e.assign_external_role(
            "provider-b",
            "shared-subject",
            "prism-bob",
            LocalRole::NodeAdmin,
        )
        .unwrap();

        assert_eq!(
            e.get_role("prism-alice").unwrap(),
            Some(LocalRole::Engineer)
        );
        assert_eq!(e.get_role("prism-bob").unwrap(), Some(LocalRole::NodeAdmin));
        assert_eq!(e.get_role("shared-subject").unwrap(), None);
    }

    #[test]
    fn external_role_revocation_is_scoped_to_provider_and_subject() {
        let e = engine();
        e.assign_role("local-alice", LocalRole::Engineer).unwrap();
        e.assign_external_role(
            "provider-a",
            "shared-subject",
            "provider-a-alice",
            LocalRole::NodeAdmin,
        )
        .unwrap();
        e.assign_external_role(
            "provider-b",
            "shared-subject",
            "provider-b-alice",
            LocalRole::Analyst,
        )
        .unwrap();
        e.assign_external_role(
            "provider-a",
            "another-subject",
            "provider-a-bob",
            LocalRole::Viewer,
        )
        .unwrap();

        assert!(
            e.revoke_external_role("provider-a", "shared-subject")
                .unwrap()
        );
        assert!(
            !e.revoke_external_role("provider-a", "shared-subject")
                .unwrap()
        );

        assert_eq!(
            e.get_external_role("provider-a", "shared-subject").unwrap(),
            None
        );
        assert_eq!(
            e.get_external_role("provider-b", "shared-subject").unwrap(),
            Some(LocalRole::Analyst)
        );
        assert_eq!(
            e.get_external_role("provider-a", "another-subject")
                .unwrap(),
            Some(LocalRole::Viewer)
        );
        assert_eq!(
            e.get_local_role("local-alice").unwrap(),
            Some(LocalRole::Engineer)
        );
    }

    #[test]
    fn external_role_revocation_rejects_ambiguous_keys() {
        let e = engine();
        e.assign_external_role("provider-a", "alice", "alice", LocalRole::NodeAdmin)
            .unwrap();

        assert!(e.revoke_external_role("", "alice").is_err());
        assert!(e.revoke_external_role("provider-a", " ").is_err());
        assert_eq!(
            e.get_external_role("provider-a", "alice").unwrap(),
            Some(LocalRole::NodeAdmin)
        );
    }

    // -- Permission checks per role ------------------------------------------

    #[test]
    fn node_admin_has_all_permissions() {
        let e = engine();
        e.assign_role("admin", LocalRole::NodeAdmin).unwrap();
        for perm in &[
            Permission::ManageNode,
            Permission::ManageUsers,
            Permission::ExecuteTools,
            Permission::PublishData,
            Permission::IngestData,
            Permission::QueryData,
            Permission::ViewDashboard,
            Permission::ViewAudit,
        ] {
            assert!(
                e.check_permission("admin", *perm).unwrap(),
                "NodeAdmin should have {:?}",
                perm
            );
        }
    }

    #[test]
    fn engineer_permissions() {
        let e = engine();
        e.assign_role("eng", LocalRole::Engineer).unwrap();

        // Should have
        for perm in &[
            Permission::ExecuteTools,
            Permission::PublishData,
            Permission::IngestData,
            Permission::QueryData,
            Permission::ViewDashboard,
        ] {
            assert!(
                e.check_permission("eng", *perm).unwrap(),
                "Engineer should have {:?}",
                perm
            );
        }
        // Should NOT have
        for perm in &[
            Permission::ManageNode,
            Permission::ManageUsers,
            Permission::ViewAudit,
        ] {
            assert!(
                !e.check_permission("eng", *perm).unwrap(),
                "Engineer should NOT have {:?}",
                perm
            );
        }
    }

    #[test]
    fn analyst_permissions() {
        let e = engine();
        e.assign_role("ana", LocalRole::Analyst).unwrap();

        for perm in &[
            Permission::QueryData,
            Permission::ViewDashboard,
            Permission::ViewAudit,
        ] {
            assert!(
                e.check_permission("ana", *perm).unwrap(),
                "Analyst should have {:?}",
                perm
            );
        }
        for perm in &[
            Permission::ManageNode,
            Permission::ManageUsers,
            Permission::ExecuteTools,
            Permission::PublishData,
            Permission::IngestData,
        ] {
            assert!(
                !e.check_permission("ana", *perm).unwrap(),
                "Analyst should NOT have {:?}",
                perm
            );
        }
    }

    #[test]
    fn viewer_permissions() {
        let e = engine();
        e.assign_role("viewer", LocalRole::Viewer).unwrap();

        assert!(
            e.check_permission("viewer", Permission::ViewDashboard)
                .unwrap()
        );

        for perm in &[
            Permission::ManageNode,
            Permission::ManageUsers,
            Permission::ExecuteTools,
            Permission::PublishData,
            Permission::IngestData,
            Permission::QueryData,
            Permission::ViewAudit,
        ] {
            assert!(
                !e.check_permission("viewer", *perm).unwrap(),
                "Viewer should NOT have {:?}",
                perm
            );
        }
    }

    #[test]
    fn no_role_denies_all() {
        let e = engine();
        assert!(
            !e.check_permission("nobody", Permission::ViewDashboard)
                .unwrap()
        );
    }

    // -- Edge cases and error paths -------------------------------------------

    #[test]
    fn empty_user_id_works() {
        let e = engine();
        e.assign_role("", LocalRole::Viewer).unwrap();
        assert_eq!(e.get_role("").unwrap(), Some(LocalRole::Viewer));
        assert!(e.check_permission("", Permission::ViewDashboard).unwrap());
    }

    #[test]
    fn unicode_user_id() {
        let e = engine();
        e.assign_role("用户🧪", LocalRole::Engineer).unwrap();
        assert_eq!(e.get_role("用户🧪").unwrap(), Some(LocalRole::Engineer));
        let users = e.list_users().unwrap();
        assert!(users.iter().any(|(uid, _)| uid == "用户🧪"));
    }

    #[test]
    fn very_long_user_id() {
        let e = engine();
        let long_id = "a".repeat(10_000);
        e.assign_role(&long_id, LocalRole::Analyst).unwrap();
        assert_eq!(e.get_role(&long_id).unwrap(), Some(LocalRole::Analyst));
    }

    #[test]
    fn remove_nonexistent_user_is_ok() {
        let e = engine();
        // Should not error — just a no-op DELETE.
        e.remove_role("ghost").unwrap();
    }

    #[test]
    fn list_users_empty_db() {
        let e = engine();
        let users = e.list_users().unwrap();
        assert!(users.is_empty());
    }

    #[test]
    fn many_users_scale() {
        let e = engine();
        for i in 0..500 {
            e.assign_role(&format!("user-{i}"), LocalRole::Viewer)
                .unwrap();
        }
        assert_eq!(e.list_users().unwrap().len(), 500);
    }

    #[test]
    fn role_serde_roundtrip() {
        for role in [
            LocalRole::NodeAdmin,
            LocalRole::Engineer,
            LocalRole::Analyst,
            LocalRole::Viewer,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let parsed: LocalRole = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn permission_serde_roundtrip() {
        for perm in [
            Permission::ManageNode,
            Permission::ManageUsers,
            Permission::ExecuteTools,
            Permission::PublishData,
            Permission::IngestData,
            Permission::QueryData,
            Permission::ViewDashboard,
            Permission::ViewAudit,
        ] {
            let json = serde_json::to_string(&perm).unwrap();
            let parsed: Permission = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, perm);
        }
    }

    #[test]
    fn local_role_from_str_invalid_returns_none() {
        assert!(LocalRole::from_str("superadmin").is_none());
        assert!(LocalRole::from_str("").is_none());
        assert!(LocalRole::from_str("NODE_ADMIN").is_none()); // case sensitive
    }

    #[test]
    fn file_backed_db_persists() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        {
            let e = RbacEngine::new(path).unwrap();
            e.assign_role("alice", LocalRole::NodeAdmin).unwrap();
        }
        // Re-open from disk.
        let e = RbacEngine::new(path).unwrap();
        assert_eq!(e.get_role("alice").unwrap(), Some(LocalRole::NodeAdmin));
    }

    #[test]
    fn pre_stage_one_roles_are_grandfathered_as_local_when_provenance_is_unknowable() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE user_roles (
                    user_id TEXT PRIMARY KEY NOT NULL,
                    role TEXT NOT NULL
                 );
                 INSERT INTO user_roles (user_id, role) VALUES ('legacy-admin', 'node_admin');",
            )
            .unwrap();
        }

        let e = RbacEngine::new(tmp.path()).unwrap();

        // Provider reconciliation cannot safely infer whether this legacy row
        // was provider-synced or administrator-created, so it is never deleted.
        e.replace_external_roles("provider-a", &[]).unwrap();
        assert_eq!(
            e.get_local_role("legacy-admin").unwrap(),
            Some(LocalRole::NodeAdmin)
        );
        assert_eq!(
            e.get_role("legacy-admin").unwrap(),
            Some(LocalRole::NodeAdmin)
        );

        e.assign_external_role("provider-a", "external-bob", "bob", LocalRole::Viewer)
            .unwrap();
        assert_eq!(e.get_role("bob").unwrap(), Some(LocalRole::Viewer));
    }
}
