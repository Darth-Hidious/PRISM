// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Container orchestration for PRISM node services.
//!
//! Manages the lifecycle of infrastructure services (Kafka, Spark,
//! Firecrawl) that a PRISM node depends on. Two modes:
//!
//! - **Managed** ([`docker`]): PRISM pulls images, starts/stops containers, and monitors health.
//! - **External** ([`external`]): User provides connection URIs to pre-existing instances.
//!
//! Uses the Docker Engine API (via `bollard`) directly — no docker-compose dependency.

pub mod docker;
pub mod external;
pub mod health;
pub mod services;

use anyhow::Result;
use async_trait::async_trait;

/// Trait for managing service lifecycle (Docker or external).
#[async_trait]
pub trait ServiceOrchestrator: Send + Sync {
    async fn start_all(&self, config: &crate::services::ServiceConfig) -> Result<ServiceHandles>;
    async fn stop_all(&self, handles: &ServiceHandles) -> Result<()>;
    async fn health_check(&self, handles: &ServiceHandles) -> Result<HealthReport>;
}

pub struct ServiceHandles {
    pub services: Vec<ServiceHandle>,
}

pub struct ServiceHandle {
    pub name: String,
    pub container_id: Option<String>,
    pub port: u16,
    pub healthy: bool,
}

pub struct HealthReport {
    pub services: Vec<ServiceHealth>,
}

pub struct ServiceHealth {
    pub name: String,
    pub status: String,
    pub port: u16,
}

impl ServiceHandles {
    /// Check if all services are healthy.
    pub fn all_healthy(&self) -> bool {
        self.services.iter().all(|s| s.healthy)
    }

    /// Get a handle by service name.
    pub fn get(&self, name: &str) -> Option<&ServiceHandle> {
        self.services.iter().find(|s| s.name == name)
    }
}

impl HealthReport {
    /// Summary string for display.
    pub fn summary(&self) -> String {
        self.services
            .iter()
            .map(|s| format!("{}:{} ({})", s.name, s.port, s.status))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// Re-exports for convenience
pub use docker::DockerOrchestrator;
pub use external::{ExternalConnector, ExternalServices};
pub use health::{HealthChecker, HealthMonitor};
pub use services::ServiceConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let cfg = ServiceConfig::default();
        // Kafka and Spark are opt-in; Firecrawl is on by default.
        assert!(cfg.kafka.is_none());
        assert!(cfg.spark.is_none());
        assert_eq!(cfg.firecrawl.as_ref().unwrap().port, 3002);
        assert!(cfg.firecrawl.as_ref().unwrap().image.contains("firecrawl"));
    }

    #[test]
    fn handles_all_healthy() {
        let handles = ServiceHandles {
            services: vec![
                ServiceHandle {
                    name: "kafka".into(),
                    container_id: Some("abc".into()),
                    port: 9092,
                    healthy: true,
                },
                ServiceHandle {
                    name: "spark".into(),
                    container_id: Some("def".into()),
                    port: 7077,
                    healthy: true,
                },
            ],
        };
        assert!(handles.all_healthy());
        assert!(handles.get("kafka").is_some());
        assert!(handles.get("redis").is_none());
    }

    #[test]
    fn handles_not_all_healthy() {
        let handles = ServiceHandles {
            services: vec![
                ServiceHandle {
                    name: "kafka".into(),
                    container_id: Some("abc".into()),
                    port: 9092,
                    healthy: true,
                },
                ServiceHandle {
                    name: "spark".into(),
                    container_id: Some("def".into()),
                    port: 7077,
                    healthy: false,
                },
            ],
        };
        assert!(!handles.all_healthy());
    }

    #[test]
    fn health_report_summary() {
        let report = HealthReport {
            services: vec![ServiceHealth {
                name: "kafka".into(),
                status: "healthy".into(),
                port: 9092,
            }],
        };
        assert_eq!(report.summary(), "kafka:9092 (healthy)");
    }
}
