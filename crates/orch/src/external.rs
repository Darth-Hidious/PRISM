//! External service connector — connects to user-provided service URIs
//! instead of managing Docker containers.
//!
//! Used when the user already has Kafka running and wants PRISM to
//! connect to it instead of starting a container.

use anyhow::Result;
use async_trait::async_trait;
use tracing::info;

use crate::health::HealthChecker;
use crate::services::ServiceConfig;
use crate::{HealthReport, ServiceHandle, ServiceHandles, ServiceHealth, ServiceOrchestrator};

/// External URIs provided by the user for pre-existing services.
#[derive(Debug, Clone, Default)]
pub struct ExternalServices {
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
        // Try to extract port from URI like "kafka://host:9092" or "host:9092"
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

        if let Some(ref uri) = self.external.kafka_uri {
            let port = Self::parse_port(uri, 9092);
            let healthy = checker.check_port(port).await;
            info!(uri, port, healthy, "external Kafka");
            if !healthy {
                anyhow::bail!("Cannot connect to external Kafka at {uri}");
            }
            services.push(ServiceHandle {
                name: "kafka".to_string(),
                container_id: None, // external — no container
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
    fn parse_port_from_scheme_uri() {
        assert_eq!(
            ExternalConnector::parse_port("kafka://localhost:9092", 9092),
            9092
        );
        assert_eq!(
            ExternalConnector::parse_port("kafka://broker.internal:9100", 9092),
            9100
        );
    }

    #[test]
    fn parse_port_from_http_uri() {
        assert_eq!(
            ExternalConnector::parse_port("http://10.0.0.5:3002", 3002),
            3002
        );
        assert_eq!(
            ExternalConnector::parse_port("http://scraper:3010/", 3002),
            3010
        );
    }

    #[test]
    fn parse_port_bare_host() {
        assert_eq!(
            ExternalConnector::parse_port("kafka-broker:9092", 9092),
            9092
        );
    }

    #[test]
    fn parse_port_falls_back_to_default() {
        assert_eq!(ExternalConnector::parse_port("just-a-hostname", 9092), 9092);
    }
}
