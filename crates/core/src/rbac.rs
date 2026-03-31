//! Role-Based Access Control engine for PRISM nodes.
//!
//! Two-layer model:
//! - **Platform roles** (`PlatformRole`): synced from platform.marc27.com
//! - **Local roles** (`LocalRole`): managed by the node admin, persisted in SQLite

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Platform roles (synced from platform.marc27.com)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformRole {
    Owner,
    Admin,
    Member,
    Viewer,
}

impl PlatformRole {
    /// Convert a platform role string (from API) to the enum.
    pub fn from_api_str(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(PlatformRole::Owner),
            "admin" => Some(PlatformRole::Admin),
            "member" => Some(PlatformRole::Member),
            "viewer" => Some(PlatformRole::Viewer),
            _ => None,
        }
    }

    /// Map a platform role to the corresponding local role.
    ///
    /// Owner/Admin → NodeAdmin, Member → Engineer, Viewer → Viewer.
    pub fn to_local_role(self) -> LocalRole {
        match self {
            PlatformRole::Owner | PlatformRole::Admin => LocalRole::NodeAdmin,
            PlatformRole::Member => LocalRole::Engineer,
            PlatformRole::Viewer => LocalRole::Viewer,
        }
    }
}

// ---------------------------------------------------------------------------
// Local roles (managed per-node)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// RBAC Engine
// ---------------------------------------------------------------------------

/// SQLite-backed RBAC engine for local role management.
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
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS user_roles (
                    user_id  TEXT PRIMARY KEY NOT NULL,
                    role     TEXT NOT NULL
                );",
            )
            .context("failed to initialize RBAC schema")?;
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

    /// Look up the local role for a user, if one is assigned.
    pub fn get_role(&self, user_id: &str) -> Result<Option<LocalRole>> {
        let mut stmt = self
            .conn
            .prepare("SELECT role FROM user_roles WHERE user_id = ?1")?;
        let role = stmt
            .query_row(params![user_id], |row| row.get::<_, String>(0))
            .optional()?
            .and_then(|s| LocalRole::from_str(&s));
        Ok(role)
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

    /// List all users and their assigned local roles.
    pub fn list_users(&self) -> Result<Vec<(String, LocalRole)>> {
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

        assert!(e
            .check_permission("viewer", Permission::ViewDashboard)
            .unwrap());

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
        assert!(!e
            .check_permission("nobody", Permission::ViewDashboard)
            .unwrap());
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
    fn platform_role_serde_roundtrip() {
        for role in [
            PlatformRole::Owner,
            PlatformRole::Admin,
            PlatformRole::Member,
            PlatformRole::Viewer,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let parsed: PlatformRole = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn platform_role_from_api_str() {
        assert_eq!(
            PlatformRole::from_api_str("owner"),
            Some(PlatformRole::Owner)
        );
        assert_eq!(
            PlatformRole::from_api_str("admin"),
            Some(PlatformRole::Admin)
        );
        assert_eq!(
            PlatformRole::from_api_str("member"),
            Some(PlatformRole::Member)
        );
        assert_eq!(
            PlatformRole::from_api_str("viewer"),
            Some(PlatformRole::Viewer)
        );
        assert_eq!(PlatformRole::from_api_str("superadmin"), None);
        assert_eq!(PlatformRole::from_api_str(""), None);
    }

    #[test]
    fn platform_to_local_role_mapping() {
        assert_eq!(PlatformRole::Owner.to_local_role(), LocalRole::NodeAdmin);
        assert_eq!(PlatformRole::Admin.to_local_role(), LocalRole::NodeAdmin);
        assert_eq!(PlatformRole::Member.to_local_role(), LocalRole::Engineer);
        assert_eq!(PlatformRole::Viewer.to_local_role(), LocalRole::Viewer);
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
}
