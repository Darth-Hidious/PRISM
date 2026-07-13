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
    ///
    /// TCP connect is the readiness signal for every managed service
    /// (Kafka and Spark have no HTTP health endpoint on their primary
    /// port; the old per-service HTTP checks belonged to the retired
    /// Neo4j/Qdrant containers).
    pub async fn wait_ready(&self, name: &str, port: u16, max_wait: Duration) -> bool {
        let start = tokio::time::Instant::now();
        loop {
            if self.check_port(port).await {
                return true;
            }

            if start.elapsed() >= max_wait {
                warn!(service = name, port, "Timed out waiting for readiness");
                return false;
            }

            debug!(service = name, port, "Not ready yet, retrying...");
            sleep(self.probe_interval).await;
        }
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
