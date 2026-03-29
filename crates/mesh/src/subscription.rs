//! Subscription management for cross-node data feeds.
//!
//! [`SubscriptionManager::new()`] creates an in-memory manager (tests, ephemeral nodes).
//! [`SubscriptionManager::open()`] creates a SQLite-backed manager that persists across restarts.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A dataset published by this node for other nodes to subscribe to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedDataset {
    pub name: String,
    pub schema_version: String,
    pub subscribers: Vec<Uuid>,
}

/// A subscription this node holds to a dataset on another node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub dataset_name: String,
    pub publisher_node: Uuid,
    pub subscribed_at: DateTime<Utc>,
}

/// Tracks published datasets and active subscriptions.
///
/// Operates in two modes:
/// - **In-memory** (`new()`) — no persistence, good for tests.
/// - **SQLite-backed** (`open()`) — survives restarts, used in production.
#[derive(Debug)]
pub struct SubscriptionManager {
    published: Vec<PublishedDataset>,
    subscriptions: Vec<Subscription>,
    /// Wrapped in Mutex so SubscriptionManager is Send (rusqlite::Connection is !Send).
    db: Option<Mutex<Connection>>,
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self {
            published: Vec::new(),
            subscriptions: Vec::new(),
            db: None,
        }
    }
}

impl Clone for SubscriptionManager {
    fn clone(&self) -> Self {
        // Clone only the in-memory state; cloned managers are always in-memory.
        Self {
            published: self.published.clone(),
            subscriptions: self.subscriptions.clone(),
            db: None,
        }
    }
}

impl SubscriptionManager {
    /// Create an in-memory subscription manager (no persistence).
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a SQLite-backed subscription manager, loading existing state.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open subscription db at {}", db_path.display()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS published (
                name TEXT PRIMARY KEY,
                schema_version TEXT NOT NULL,
                subscribers TEXT NOT NULL DEFAULT '[]'
            );
            CREATE TABLE IF NOT EXISTS subscriptions (
                dataset_name TEXT NOT NULL,
                publisher_node TEXT NOT NULL,
                subscribed_at TEXT NOT NULL,
                PRIMARY KEY (dataset_name, publisher_node)
            );",
        )
        .context("failed to create subscription tables")?;

        let published = {
            let mut stmt = conn
                .prepare("SELECT name, schema_version, subscribers FROM published")
                .context("failed to prepare published query")?;
            let rows = stmt
                .query_map([], |row| {
                    let name: String = row.get(0)?;
                    let schema_version: String = row.get(1)?;
                    let subs_json: String = row.get(2)?;
                    let subscribers: Vec<Uuid> =
                        serde_json::from_str(&subs_json).unwrap_or_default();
                    Ok(PublishedDataset {
                        name,
                        schema_version,
                        subscribers,
                    })
                })
                .context("failed to query published datasets")?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let subscriptions = {
            let mut stmt = conn
                .prepare("SELECT dataset_name, publisher_node, subscribed_at FROM subscriptions")
                .context("failed to prepare subscriptions query")?;
            let rows = stmt
                .query_map([], |row| {
                    let dataset_name: String = row.get(0)?;
                    let publisher_str: String = row.get(1)?;
                    let at_str: String = row.get(2)?;
                    let publisher_node =
                        Uuid::parse_str(&publisher_str).unwrap_or_else(|_| Uuid::nil());
                    let subscribed_at = DateTime::parse_from_rfc3339(&at_str)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now());
                    Ok(Subscription {
                        dataset_name,
                        publisher_node,
                        subscribed_at,
                    })
                })
                .context("failed to query subscriptions")?;
            rows.filter_map(|r| r.ok()).collect()
        };

        Ok(Self {
            published,
            subscriptions,
            db: Some(Mutex::new(conn)),
        })
    }

    /// Register a dataset as published by this node.
    pub fn publish(&mut self, dataset: PublishedDataset) {
        if let Some(ref db) = self.db { let conn = db.lock().unwrap_or_else(|e| e.into_inner());
            let subs_json = serde_json::to_string(&dataset.subscribers).unwrap_or_default();
            conn.execute(
                "INSERT OR REPLACE INTO published (name, schema_version, subscribers) VALUES (?1, ?2, ?3)",
                rusqlite::params![dataset.name, dataset.schema_version, subs_json],
            )
            .ok();
        }
        self.published.push(dataset);
    }

    /// Remove a published dataset by name.
    pub fn unpublish(&mut self, name: &str) {
        if let Some(ref db) = self.db { let conn = db.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute("DELETE FROM published WHERE name = ?1", rusqlite::params![name])
                .ok();
        }
        self.published.retain(|d| d.name != name);
    }

    /// Track a subscription to a remote dataset.
    pub fn subscribe(&mut self, sub: Subscription) {
        if let Some(ref db) = self.db { let conn = db.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute(
                "INSERT OR REPLACE INTO subscriptions (dataset_name, publisher_node, subscribed_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    sub.dataset_name,
                    sub.publisher_node.to_string(),
                    sub.subscribed_at.to_rfc3339(),
                ],
            )
            .ok();
        }
        self.subscriptions.push(sub);
    }

    /// Remove a subscription by dataset name and publisher node.
    pub fn unsubscribe(&mut self, dataset_name: &str, publisher: Uuid) {
        if let Some(ref db) = self.db { let conn = db.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute(
                "DELETE FROM subscriptions WHERE dataset_name = ?1 AND publisher_node = ?2",
                rusqlite::params![dataset_name, publisher.to_string()],
            )
            .ok();
        }
        self.subscriptions
            .retain(|s| !(s.dataset_name == dataset_name && s.publisher_node == publisher));
    }

    /// All datasets published by this node.
    pub fn published(&self) -> &[PublishedDataset] {
        &self.published
    }

    /// Mutable access to published datasets (for updating subscriber lists).
    pub fn published_mut(&mut self) -> &mut [PublishedDataset] {
        &mut self.published
    }

    /// All active subscriptions.
    pub fn subscriptions(&self) -> &[Subscription] {
        &self.subscriptions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn published_dataset_serde_roundtrip() {
        let ds = PublishedDataset {
            name: "alloy-db".into(),
            schema_version: "1.0".into(),
            subscribers: vec![Uuid::new_v4(), Uuid::new_v4()],
        };
        let json = serde_json::to_string(&ds).unwrap();
        let back: PublishedDataset = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, ds.name);
        assert_eq!(back.schema_version, ds.schema_version);
        assert_eq!(back.subscribers.len(), 2);
        assert_eq!(back.subscribers, ds.subscribers);
    }

    #[test]
    fn subscription_serde_roundtrip() {
        let publisher = Uuid::new_v4();
        let sub = Subscription {
            dataset_name: "phase-diagrams".into(),
            publisher_node: publisher,
            subscribed_at: Utc::now(),
        };
        let json = serde_json::to_string(&sub).unwrap();
        let back: Subscription = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dataset_name, sub.dataset_name);
        assert_eq!(back.publisher_node, publisher);
    }

    #[test]
    fn unpublish_nonexistent_is_noop() {
        let mut mgr = SubscriptionManager::new();
        mgr.unpublish("does-not-exist");
        assert!(mgr.published().is_empty());

        mgr.publish(PublishedDataset {
            name: "real-dataset".into(),
            schema_version: "1.0".into(),
            subscribers: vec![],
        });
        mgr.unpublish("does-not-exist");
        assert_eq!(mgr.published().len(), 1);
    }

    #[test]
    fn unsubscribe_nonexistent_is_noop() {
        let mut mgr = SubscriptionManager::new();
        let phantom = Uuid::new_v4();
        mgr.unsubscribe("ghost", phantom);
        assert!(mgr.subscriptions().is_empty());

        let real_node = Uuid::new_v4();
        mgr.subscribe(Subscription {
            dataset_name: "alloy-db".into(),
            publisher_node: real_node,
            subscribed_at: Utc::now(),
        });
        mgr.unsubscribe("ghost", phantom);
        assert_eq!(mgr.subscriptions().len(), 1);
    }

    #[test]
    fn publish_duplicate_names_both_retained() {
        let mut mgr = SubscriptionManager::new();
        mgr.publish(PublishedDataset {
            name: "alloy-db".into(),
            schema_version: "1.0".into(),
            subscribers: vec![],
        });
        mgr.publish(PublishedDataset {
            name: "alloy-db".into(),
            schema_version: "2.0".into(),
            subscribers: vec![],
        });
        assert_eq!(mgr.published().len(), 2);
        mgr.unpublish("alloy-db");
        assert!(mgr.published().is_empty());
    }

    #[test]
    fn subscription_manager_new_starts_empty() {
        let mgr = SubscriptionManager::new();
        assert!(mgr.published().is_empty());
        assert!(mgr.subscriptions().is_empty());
    }

    // ── SQLite persistence tests ──────────────────────────────────────

    #[test]
    fn sqlite_publish_persists_across_reopen() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        {
            let mut mgr = SubscriptionManager::open(path).unwrap();
            mgr.publish(PublishedDataset {
                name: "alloy-db".into(),
                schema_version: "1.0".into(),
                subscribers: vec![],
            });
            mgr.publish(PublishedDataset {
                name: "phase-diagrams".into(),
                schema_version: "2.1".into(),
                subscribers: vec![],
            });
        }

        let mgr2 = SubscriptionManager::open(path).unwrap();
        assert_eq!(mgr2.published().len(), 2);
        assert!(mgr2.published().iter().any(|d| d.name == "alloy-db"));
        assert!(mgr2.published().iter().any(|d| d.name == "phase-diagrams"));
    }

    #[test]
    fn sqlite_subscribe_persists_across_reopen() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let node_a = Uuid::new_v4();
        let node_b = Uuid::new_v4();

        {
            let mut mgr = SubscriptionManager::open(path).unwrap();
            mgr.subscribe(Subscription {
                dataset_name: "alloy-db".into(),
                publisher_node: node_a,
                subscribed_at: Utc::now(),
            });
            mgr.subscribe(Subscription {
                dataset_name: "alloy-db".into(),
                publisher_node: node_b,
                subscribed_at: Utc::now(),
            });
        }

        let mgr2 = SubscriptionManager::open(path).unwrap();
        assert_eq!(mgr2.subscriptions().len(), 2);
    }

    #[test]
    fn sqlite_unpublish_persists() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        {
            let mut mgr = SubscriptionManager::open(path).unwrap();
            mgr.publish(PublishedDataset {
                name: "alloy-db".into(),
                schema_version: "1.0".into(),
                subscribers: vec![],
            });
            mgr.unpublish("alloy-db");
        }

        let mgr2 = SubscriptionManager::open(path).unwrap();
        assert!(mgr2.published().is_empty());
    }

    #[test]
    fn sqlite_unsubscribe_persists() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let node = Uuid::new_v4();

        {
            let mut mgr = SubscriptionManager::open(path).unwrap();
            mgr.subscribe(Subscription {
                dataset_name: "alloy-db".into(),
                publisher_node: node,
                subscribed_at: Utc::now(),
            });
            mgr.unsubscribe("alloy-db", node);
        }

        let mgr2 = SubscriptionManager::open(path).unwrap();
        assert!(mgr2.subscriptions().is_empty());
    }
}
