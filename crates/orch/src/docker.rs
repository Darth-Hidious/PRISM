//! Docker-based service orchestrator using bollard.
//!
//! Manages the lifecycle of PRISM's managed services (Neo4j, Qdrant, Kafka)
//! as Docker containers with a `prism-` prefix for easy identification.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, PortBinding};
use bollard::Docker;
use futures_util::StreamExt;
use tracing::{debug, info, warn};

use crate::health::HealthChecker;
use crate::services::ServiceConfig;
use crate::{HealthReport, ServiceHandle, ServiceHandles, ServiceOrchestrator};

const CONTAINER_PREFIX: &str = "prism-";
const LABEL_KEY: &str = "io.marc27.prism";
const LABEL_VALUE: &str = "managed";

/// Docker-based service orchestrator using bollard.
pub struct DockerOrchestrator {
    docker: Docker,
}

impl DockerOrchestrator {
    /// Connect to the local Docker daemon.
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("Failed to connect to Docker — is Docker running?")?;
        Ok(Self { docker })
    }

    /// Connect with a specific Docker host URI.
    pub fn with_uri(uri: &str) -> Result<Self> {
        let docker = Docker::connect_with_http(uri, 10, bollard::API_DEFAULT_VERSION)
            .context("Failed to connect to Docker")?;
        Ok(Self { docker })
    }

    /// Pull an image if not already present locally.
    async fn ensure_image(&self, image: &str) -> Result<()> {
        // Check if image exists locally
        match self.docker.inspect_image(image).await {
            Ok(_) => {
                debug!(image, "Image already present");
                return Ok(());
            }
            Err(_) => {
                info!(image, "Pulling image...");
            }
        }

        let (repo, tag) = if let Some(pos) = image.rfind(':') {
            (&image[..pos], &image[pos + 1..])
        } else {
            (image, "latest")
        };

        let opts = CreateImageOptions {
            from_image: repo,
            tag,
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(opts), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!(status, "pull progress");
                    }
                }
                Err(e) => bail!("Failed to pull {image}: {e}"),
            }
        }

        info!(image, "Image pulled successfully");
        Ok(())
    }

    /// Create and start a container. Returns the container ID.
    async fn run_container(
        &self,
        name: &str,
        image: &str,
        env: Vec<String>,
        port_bindings: HashMap<String, Option<Vec<PortBinding>>>,
        volumes: Option<HashMap<String, HashMap<(), ()>>>,
    ) -> Result<String> {
        let container_name = format!("{CONTAINER_PREFIX}{name}");

        // Remove existing container with same name (stale from previous run)
        if self.container_exists(&container_name).await {
            info!(container_name, "Removing stale container");
            self.docker
                .remove_container(
                    &container_name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
                .ok();
        }

        let mut labels = HashMap::new();
        labels.insert(LABEL_KEY.to_string(), LABEL_VALUE.to_string());

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            ..Default::default()
        };

        let config = Config {
            image: Some(image.to_string()),
            env: Some(env),
            labels: Some(labels),
            host_config: Some(host_config),
            volumes,
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: container_name.clone(),
            platform: None,
        };

        let response = self
            .docker
            .create_container(Some(opts), config)
            .await
            .with_context(|| format!("Failed to create container {container_name}"))?;

        self.docker
            .start_container(&container_name, None::<StartContainerOptions<String>>)
            .await
            .with_context(|| format!("Failed to start container {container_name}"))?;

        info!(container_name, id = %response.id, "Container started");
        Ok(response.id)
    }

    /// Check if a container with the given name exists.
    async fn container_exists(&self, name: &str) -> bool {
        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name.to_string()]);
        let opts = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };
        self.docker
            .list_containers(Some(opts))
            .await
            .map(|list| !list.is_empty())
            .unwrap_or(false)
    }

    /// Start Neo4j and return a ServiceHandle.
    async fn start_neo4j(&self, config: &crate::services::Neo4jConfig) -> Result<ServiceHandle> {
        self.ensure_image(&config.image).await?;

        let env = vec![
            "NEO4J_AUTH=neo4j/prism-local".to_string(),
            "NEO4J_PLUGINS=[\"apoc\"]".to_string(),
            // Allocate reasonable memory for local dev
            "NEO4J_server_memory_heap_initial__size=256m".to_string(),
            "NEO4J_server_memory_heap_max__size=512m".to_string(),
        ];

        let mut port_bindings = HashMap::new();
        // Bolt protocol
        port_bindings.insert(
            "7687/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(config.bolt_port.to_string()),
            }]),
        );
        // HTTP browser
        port_bindings.insert(
            "7474/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(config.http_port.to_string()),
            }]),
        );

        let container_id = self
            .run_container("neo4j", &config.image, env, port_bindings, None)
            .await?;

        Ok(ServiceHandle {
            name: "neo4j".to_string(),
            container_id: Some(container_id),
            port: config.bolt_port,
            healthy: false, // will be checked separately
        })
    }

    /// Start Qdrant and return a ServiceHandle.
    async fn start_qdrant(
        &self,
        config: &crate::services::VectorDbConfig,
    ) -> Result<ServiceHandle> {
        self.ensure_image(&config.image).await?;

        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            "6333/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(config.port.to_string()),
            }]),
        );
        // gRPC port
        port_bindings.insert(
            "6334/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some((config.port + 1).to_string()),
            }]),
        );

        let container_id = self
            .run_container("qdrant", &config.image, vec![], port_bindings, None)
            .await?;

        Ok(ServiceHandle {
            name: "qdrant".to_string(),
            container_id: Some(container_id),
            port: config.port,
            healthy: false,
        })
    }

    /// Start Kafka (KRaft mode, no Zookeeper) and return a ServiceHandle.
    async fn start_kafka(&self, config: &crate::services::KafkaConfig) -> Result<ServiceHandle> {
        self.ensure_image(&config.image).await?;

        let env = vec![
            // KRaft mode — no Zookeeper needed
            "KAFKA_NODE_ID=1".to_string(),
            "KAFKA_PROCESS_ROLES=broker,controller".to_string(),
            "KAFKA_CONTROLLER_QUORUM_VOTERS=1@localhost:9093".to_string(),
            format!(
                "KAFKA_LISTENERS=PLAINTEXT://0.0.0.0:{},CONTROLLER://0.0.0.0:9093",
                config.port
            ),
            format!(
                "KAFKA_ADVERTISED_LISTENERS=PLAINTEXT://localhost:{}",
                config.port
            ),
            "KAFKA_CONTROLLER_LISTENER_NAMES=CONTROLLER".to_string(),
            "KAFKA_LISTENER_SECURITY_PROTOCOL_MAP=CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT"
                .to_string(),
            "CLUSTER_ID=prism-local-cluster-001".to_string(),
        ];

        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            format!("{}/tcp", config.port),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(config.port.to_string()),
            }]),
        );

        let container_id = self
            .run_container("kafka", &config.image, env, port_bindings, None)
            .await?;

        Ok(ServiceHandle {
            name: "kafka".to_string(),
            container_id: Some(container_id),
            port: config.port,
            healthy: false,
        })
    }

    /// Stop and remove a container by ID.
    async fn stop_container(&self, container_id: &str) -> Result<()> {
        self.docker
            .stop_container(container_id, Some(StopContainerOptions { t: 10 }))
            .await
            .ok(); // Ignore if already stopped

        self.docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .with_context(|| format!("Failed to remove container {container_id}"))?;

        Ok(())
    }

    // -- Public wrappers for HealthMonitor restart --

    /// Restart Neo4j (public, used by health monitor).
    pub async fn start_neo4j_public(
        &self,
        config: &crate::services::Neo4jConfig,
    ) -> Result<ServiceHandle> {
        self.start_neo4j(config).await
    }

    /// Restart Qdrant (public, used by health monitor).
    pub async fn start_qdrant_public(
        &self,
        config: &crate::services::VectorDbConfig,
    ) -> Result<ServiceHandle> {
        self.start_qdrant(config).await
    }

    /// Restart Kafka (public, used by health monitor).
    pub async fn start_kafka_public(
        &self,
        config: &crate::services::KafkaConfig,
    ) -> Result<ServiceHandle> {
        self.start_kafka(config).await
    }

    /// Fetch stdout/stderr logs from a managed container.
    pub async fn container_logs(&self, service_name: &str, tail: usize) -> Result<String> {
        let container_name = format!("{CONTAINER_PREFIX}{service_name}");
        use bollard::container::LogsOptions;
        let opts = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            ..Default::default()
        };
        let mut stream = self.docker.logs(&container_name, Some(opts));
        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(log) => output.push_str(&log.to_string()),
                Err(e) => {
                    if output.is_empty() {
                        anyhow::bail!("Failed to read logs for {service_name}: {e}");
                    }
                    break;
                }
            }
        }
        Ok(output)
    }

    /// List all PRISM-managed containers.
    pub async fn list_managed(&self) -> Result<Vec<String>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![format!("{LABEL_KEY}={LABEL_VALUE}")],
        );
        let opts = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };
        let containers = self.docker.list_containers(Some(opts)).await?;
        Ok(containers.into_iter().filter_map(|c| c.id).collect())
    }
}

#[async_trait]
impl ServiceOrchestrator for DockerOrchestrator {
    async fn start_all(&self, config: &ServiceConfig) -> Result<ServiceHandles> {
        let mut services = Vec::new();

        // Neo4j — always required
        info!("Starting Neo4j...");
        let neo4j = self.start_neo4j(&config.neo4j).await?;
        services.push(neo4j);

        // Qdrant
        info!("Starting Qdrant...");
        let qdrant = self.start_qdrant(&config.vector_db).await?;
        services.push(qdrant);

        // Kafka — optional
        if let Some(ref kafka_cfg) = config.kafka {
            info!("Starting Kafka (KRaft)...");
            let kafka = self.start_kafka(kafka_cfg).await?;
            services.push(kafka);
        }

        // Wait for services to become healthy
        let checker = HealthChecker::new();
        for handle in &mut services {
            let healthy = checker
                .wait_ready(&handle.name, handle.port, Duration::from_secs(60))
                .await;
            handle.healthy = healthy;
            if healthy {
                info!(service = %handle.name, port = handle.port, "Service ready");
            } else {
                warn!(service = %handle.name, "Service did not become ready in time");
            }
        }

        Ok(ServiceHandles { services })
    }

    async fn stop_all(&self, handles: &ServiceHandles) -> Result<()> {
        for handle in &handles.services {
            if let Some(ref id) = handle.container_id {
                info!(service = %handle.name, "Stopping...");
                self.stop_container(id).await?;
            }
        }
        Ok(())
    }

    async fn health_check(&self, handles: &ServiceHandles) -> Result<HealthReport> {
        let checker = HealthChecker::new();
        let mut report = Vec::new();
        for handle in &handles.services {
            let ok = checker.check_port(handle.port).await;
            report.push(crate::ServiceHealth {
                name: handle.name.clone(),
                status: if ok { "healthy" } else { "down" }.to_string(),
                port: handle.port,
            });
        }
        Ok(HealthReport { services: report })
    }
}
