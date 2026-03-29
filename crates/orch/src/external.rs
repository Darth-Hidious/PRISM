//! External service connector — connects to user-provided service URIs
//! instead of managing Docker containers.
//!
//! Used when the user already has Neo4j, Qdrant, or Kafka running and wants
//! PRISM to connect to them: `prism node up --external-neo4j bolt://...`

use anyhow::Result;
use async_trait::async_trait;
use tracing::info;

use crate::health::HealthChecker;
use crate::services::ServiceConfig;
use crate::{HealthReport, ServiceHandle, ServiceHandles, ServiceHealth, ServiceOrchestrator};

/// External URIs provided by the user for pre-existing services.
#[derive(Debug, Clone, Default)]
pub struct ExternalServices {
    /// `bolt://host:port` or just `host:port` for Neo4j.
    pub neo4j_uri: Option<String>,
    /// `http://host:port` for Qdrant.
    pub qdrant_uri: Option<String>,
    /// `host:port` for Kafka broker.
    pub kafka_uri: Option<String>,
}

/// Connects to externally-managed services rather than starting Docker containers.
pub struct ExternalConnector {
    pub external: ExternalServices,
}

impl ExternalConnector {
    pub fn new(external: ExternalServices) -> Self {
        Self { external }
    }

    fn parse_port(uri: &str, default_port: u16) -> u16 {
        // Try to extract port from URI like "bolt://host:7687" or "host:7687"
        uri.rsplit(':')
            .next()
            .and_then(|p| p.trim_end_matches('/').parse().ok())
            .unwrap_or(default_port)
    }
}

#[async_trait]
impl ServiceOrchestrator for ExternalConnector {
    async fn start_all(&self, _config: &ServiceConfig) -> Result<ServiceHandles> {
        let checker = HealthChecker::new();
        let mut services = Vec::new();

        if let Some(ref uri) = self.external.neo4j_uri {
            let port = Self::parse_port(uri, 7687);
            let healthy = checker.check_port(port).await;
            info!(uri, port, healthy, "external Neo4j");
            if !healthy {
                anyhow::bail!("Cannot connect to external Neo4j at {uri}");
            }
            services.push(ServiceHandle {
                name: "neo4j".to_string(),
                container_id: None, // external — no container
                port,
                healthy,
            });
        }

        if let Some(ref uri) = self.external.qdrant_uri {
            let port = Self::parse_port(uri, 6333);
            let healthy = checker.check_port(port).await;
            info!(uri, port, healthy, "external Qdrant");
            if !healthy {
                anyhow::bail!("Cannot connect to external Qdrant at {uri}");
            }
            services.push(ServiceHandle {
                name: "qdrant".to_string(),
                container_id: None,
                port,
                healthy,
            });
        }

        if let Some(ref uri) = self.external.kafka_uri {
            let port = Self::parse_port(uri, 9092);
            let healthy = checker.check_port(port).await;
            info!(uri, port, healthy, "external Kafka");
            if !healthy {
                anyhow::bail!("Cannot connect to external Kafka at {uri}");
            }
            services.push(ServiceHandle {
                name: "kafka".to_string(),
                container_id: None,
                port,
                healthy,
            });
        }

        Ok(ServiceHandles { services })
    }

    async fn stop_all(&self, _handles: &ServiceHandles) -> Result<()> {
        // External services are not managed by us — nothing to stop.
        info!("External services are not managed — nothing to stop");
        Ok(())
    }

    async fn health_check(&self, handles: &ServiceHandles) -> Result<HealthReport> {
        let checker = HealthChecker::new();
        let mut report = Vec::new();
        for handle in &handles.services {
            let ok = checker.check_port(handle.port).await;
            report.push(ServiceHealth {
                name: handle.name.clone(),
                status: if ok { "healthy" } else { "unreachable" }.to_string(),
                port: handle.port,
            });
        }
        Ok(HealthReport { services: report })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_from_bolt_uri() {
        assert_eq!(ExternalConnector::parse_port("bolt://localhost:7687", 7687), 7687);
        assert_eq!(ExternalConnector::parse_port("bolt://db.internal:7700", 7687), 7700);
    }

    #[test]
    fn parse_port_from_http_uri() {
        assert_eq!(ExternalConnector::parse_port("http://10.0.0.5:6333", 6333), 6333);
        assert_eq!(ExternalConnector::parse_port("http://qdrant:6400/", 6333), 6400);
    }

    #[test]
    fn parse_port_bare_host() {
        assert_eq!(ExternalConnector::parse_port("kafka-broker:9092", 9092), 9092);
    }

    #[test]
    fn parse_port_falls_back_to_default() {
        assert_eq!(ExternalConnector::parse_port("just-a-hostname", 7687), 7687);
    }
}
