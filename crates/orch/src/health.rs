//! Health checking for managed services.
//!
//! Uses TCP connect probes (fast) and optional HTTP checks (thorough).

use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tracing::{debug, warn};

/// Health checker for managed services.
pub struct HealthChecker {
    /// Interval between readiness probes.
    pub probe_interval: Duration,
}

impl HealthChecker {
    pub fn new() -> Self {
        Self {
            probe_interval: Duration::from_secs(2),
        }
    }

    /// Check if a TCP port is accepting connections.
    pub async fn check_port(&self, port: u16) -> bool {
        let addr = format!("127.0.0.1:{port}");
        timeout(Duration::from_secs(2), TcpStream::connect(&addr))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Wait for a service to become ready, with retries up to `max_wait`.
    pub async fn wait_ready(&self, name: &str, port: u16, max_wait: Duration) -> bool {
        let start = tokio::time::Instant::now();
        loop {
            if self.check_port(port).await {
                // For services with HTTP endpoints, do an extra HTTP check
                if self.http_ready(name, port).await {
                    return true;
                }
            }

            if start.elapsed() >= max_wait {
                warn!(service = name, port, "Timed out waiting for readiness");
                return false;
            }

            debug!(service = name, port, "Not ready yet, retrying...");
            sleep(self.probe_interval).await;
        }
    }

    /// Service-specific HTTP readiness check. Falls back to TCP-only for unknown services.
    async fn http_ready(&self, name: &str, port: u16) -> bool {
        match name {
            "neo4j" => {
                // Neo4j HTTP API — bolt port is separate, but we check the HTTP port (port - 213 = 7474 from 7687)
                // Actually, just check TCP on the bolt port — Neo4j is ready when bolt accepts connections
                true // TCP connect succeeded, good enough for bolt
            }
            "qdrant" => {
                // Qdrant has a health endpoint on its REST port
                let url = format!("http://127.0.0.1:{port}/healthz");
                Self::http_get_ok(&url).await
            }
            "kafka" => {
                // Kafka doesn't have HTTP — TCP connect on broker port is sufficient
                true
            }
            "spark" => {
                // Spark master has a web UI — TCP connect on master port is sufficient
                true
            }
            _ => true,
        }
    }

    /// Simple HTTP GET check — returns true if status is 2xx.
    async fn http_get_ok(url: &str) -> bool {
        let result = timeout(Duration::from_secs(3), async {
            // Use a minimal TCP-based HTTP/1.1 GET to avoid pulling in reqwest
            let url_parsed: Result<url::Url, _> = url.parse();
            let Ok(parsed) = url_parsed else { return false };
            let host = parsed.host_str().unwrap_or("127.0.0.1");
            let port = parsed.port().unwrap_or(80);
            let path = parsed.path();

            let Ok(mut stream) = TcpStream::connect(format!("{host}:{port}")).await else {
                return false;
            };

            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
            if stream.write_all(request.as_bytes()).await.is_err() {
                return false;
            }

            let mut buf = [0u8; 32];
            let Ok(n) = stream.read(&mut buf).await else {
                return false;
            };
            let response = String::from_utf8_lossy(&buf[..n]);
            // Check for "HTTP/1.1 200" or "HTTP/1.0 200"
            response.contains(" 200 ")
        })
        .await;

        result.unwrap_or(false)
    }
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// Background health monitor that periodically checks services and restarts
/// crashed containers via the Docker orchestrator.
pub struct HealthMonitor;

impl HealthMonitor {
    /// Spawn a background task that checks service health every `interval` and
    /// attempts to restart any containers that are down.
    ///
    /// Returns a `JoinHandle` — drop or abort it to stop monitoring.
    pub fn spawn(
        orch: std::sync::Arc<crate::docker::DockerOrchestrator>,
        config: crate::services::ServiceConfig,
        handles: std::sync::Arc<tokio::sync::RwLock<crate::ServiceHandles>>,
        interval: Duration,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let checker = HealthChecker::new();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!("health monitor shutting down");
                        return;
                    }
                    _ = sleep(interval) => {}
                }

                let current = handles.read().await;
                for handle in &current.services {
                    let ok = checker.check_port(handle.port).await;
                    if ok {
                        continue;
                    }

                    warn!(
                        service = %handle.name,
                        port = handle.port,
                        "service down — attempting restart"
                    );

                    // Attempt restart via the orchestrator
                    let restart_result = match handle.name.as_str() {
                        "neo4j" => {
                            if let Some(ref neo4j_cfg) = config.neo4j {
                                orch.start_neo4j_public(neo4j_cfg).await
                            } else {
                                continue;
                            }
                        }
                        "qdrant" => {
                            if let Some(ref vector_cfg) = config.vector_db {
                                orch.start_qdrant_public(vector_cfg).await
                            } else {
                                continue;
                            }
                        }
                        "kafka" => {
                            if let Some(ref kfg) = config.kafka {
                                orch.start_kafka_public(kfg).await
                            } else {
                                continue;
                            }
                        }
                        "spark" => {
                            if let Some(ref sfg) = config.spark {
                                orch.start_spark_public(sfg).await
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    };

                    match restart_result {
                        Ok(new_handle) => {
                            tracing::info!(
                                service = %new_handle.name,
                                "service restarted successfully"
                            );
                            // Update the handle in the shared state
                            drop(current);
                            let mut writer = handles.write().await;
                            if let Some(existing) = writer
                                .services
                                .iter_mut()
                                .find(|s| s.name == new_handle.name)
                            {
                                *existing = new_handle;
                            }
                            break; // Re-read handles on next iteration
                        }
                        Err(e) => {
                            tracing::error!(
                                service = %handle.name,
                                error = %e,
                                "failed to restart service"
                            );
                        }
                    }
                }
            }
        })
    }
}
