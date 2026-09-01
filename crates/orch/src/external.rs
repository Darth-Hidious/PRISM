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

    /// Host and port of a service URI — `kafka://host:9092`, `host:9092`,
    /// `host`, or a bare `9092` (host omitted means this machine). The host
    /// is what gets probed: it is the one thing the URI is for.
    fn parse_host_port(uri: &str, default_port: u16) -> (String, u16) {
        let rest = uri.split_once("://").map_or(uri, |(_, rest)| rest);
        let rest = rest.trim_end_matches('/');
        match rest.rsplit_once(':') {
            Some((host, port)) => match port.parse() {
                Ok(port) => (
                    if host.is_empty() { "127.0.0.1" } else { host }.to_string(),
                    port,
                ),
                Err(_) => (rest.to_string(), default_port),
            },
            None => match rest.parse() {
                Ok(port) => ("127.0.0.1".to_string(), port),
                Err(_) => (rest.to_string(), default_port),
            },
        }
    }
}

#[async_trait]
impl ServiceOrchestrator for ExternalConnector {
    async fn start_all(&self, _config: &ServiceConfig) -> Result<ServiceHandles> {
        let checker = HealthChecker::new();
        let mut services = Vec::new();

        if let Some(ref uri) = self.external.kafka_uri {
            let (host, port) = Self::parse_host_port(uri, 9092);
            let healthy = checker.check_addr(&host, port).await;
            info!(uri, host, port, healthy, "external Kafka");
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

    /// The bug this pins: something listening on loopback made a broker on
    /// another host look healthy, and a broker that was up looked down.
    #[tokio::test]
    async fn a_remote_broker_is_probed_where_it_lives() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = ServiceConfig::default();
        let remote = ExternalConnector::new(ExternalServices {
            kafka_uri: Some(format!("kafka://192.0.2.1:{port}")),
        });
        assert!(
            remote.start_all(&config).await.is_err(),
            "a loopback listener must not vouch for kafka://192.0.2.1"
        );
        let local = ExternalConnector::new(ExternalServices {
            kafka_uri: Some(format!("127.0.0.1:{port}")),
        });
        let handles = local.start_all(&config).await.unwrap();
        assert!(
            handles
                .services
                .iter()
                .any(|h| h.name == "kafka" && h.healthy)
        );
    }

    #[test]
    fn the_host_in_the_uri_is_the_host_that_gets_probed() {
        let p = ExternalConnector::parse_host_port;
        assert_eq!(
            p("kafka://broker.example:9093/", 9092),
            ("broker.example".into(), 9093)
        );
        assert_eq!(
            p("broker.example:9093", 9092),
            ("broker.example".into(), 9093)
        );
        assert_eq!(p("broker.example", 9092), ("broker.example".into(), 9092));
        assert_eq!(p("9093", 9092), ("127.0.0.1".into(), 9093));
        assert_eq!(p(":9093", 9092), ("127.0.0.1".into(), 9093));
    }

    #[test]
    fn parse_port_from_scheme_uri() {
        assert_eq!(
            ExternalConnector::parse_host_port("kafka://localhost:9092", 9092).1,
            9092
        );
        assert_eq!(
            ExternalConnector::parse_host_port("kafka://broker.internal:9100", 9092).1,
            9100
        );
    }

    #[test]
    fn parse_port_from_http_uri() {
        assert_eq!(
            ExternalConnector::parse_host_port("http://10.0.0.5:3002", 3002).1,
            3002
        );
        assert_eq!(
            ExternalConnector::parse_host_port("http://scraper:3010/", 3002).1,
            3010
        );
    }

    #[test]
    fn parse_port_bare_host() {
        assert_eq!(
            ExternalConnector::parse_host_port("kafka-broker:9092", 9092).1,
            9092
        );
    }

    #[test]
    fn parse_port_falls_back_to_default() {
        assert_eq!(
            ExternalConnector::parse_host_port("just-a-hostname", 9092).1,
            9092
        );
    }
}
