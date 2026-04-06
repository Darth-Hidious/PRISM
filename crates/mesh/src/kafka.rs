//! Kafka-backed pub/sub for mesh messages.
//!
//! Provides a producer for publishing [`MeshMessage`]s to Kafka topics
//! and a consumer that deserialises them back into a tokio channel.

use anyhow::{Context, Result};
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, TopicReplication};
use rdkafka::client::DefaultClientContext;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::Message;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::protocol::MeshMessage;

/// Configuration for the Kafka transport layer.
#[derive(Debug, Clone)]
pub struct KafkaConfig {
    pub brokers: String,
    pub topic_prefix: String,
    pub group_id: String,
}

impl Default for KafkaConfig {
    fn default() -> Self {
        Self {
            brokers: "127.0.0.1:9092".into(),
            topic_prefix: "prism.mesh".into(),
            group_id: "prism-node".into(),
        }
    }
}

/// Kafka producer for publishing mesh messages.
pub struct MeshKafkaProducer {
    producer: FutureProducer,
    topic_prefix: String,
}

impl MeshKafkaProducer {
    /// Create a new Kafka producer.
    pub fn new(config: &KafkaConfig) -> Result<Self> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", &config.brokers)
            .set("message.timeout.ms", "5000")
            .create()
            .context("failed to create Kafka producer")?;

        info!(brokers = %config.brokers, "Kafka producer connected");
        Ok(Self {
            producer,
            topic_prefix: config.topic_prefix.clone(),
        })
    }

    /// Publish a mesh message to the appropriate topic.
    pub async fn publish(&self, msg: &MeshMessage) -> Result<()> {
        let topic = self.topic_for(msg);
        let payload = serde_json::to_string(msg).context("failed to serialize mesh message")?;

        debug!(%topic, "publishing mesh message");
        self.producer
            .send(
                FutureRecord::<str, str>::to(&topic).payload(&payload),
                std::time::Duration::from_secs(5),
            )
            .await
            .map_err(|(e, _)| anyhow::anyhow!("Kafka produce failed: {e}"))?;

        Ok(())
    }

    fn topic_for(&self, msg: &MeshMessage) -> String {
        let suffix = match msg {
            MeshMessage::Announce { .. } => "announce",
            MeshMessage::Goodbye { .. } => "goodbye",
            MeshMessage::DataPublish { .. } => "data-publish",
            MeshMessage::DataSubscribe { .. } => "data-subscribe",
            MeshMessage::DataUnsubscribe { .. } => "data-unsubscribe",
            MeshMessage::QueryForward { .. } => "query-forward",
            MeshMessage::QueryResult { .. } => "query-result",
            MeshMessage::Ping { .. } => "ping",
            MeshMessage::Pong { .. } => "pong",
        };
        format!("{}.{suffix}", self.topic_prefix)
    }
}

/// Kafka consumer that feeds mesh messages into a channel.
pub struct MeshKafkaConsumer {
    consumer: StreamConsumer,
    brokers: String,
    topic_prefix: String,
}

impl MeshKafkaConsumer {
    /// Create a new Kafka consumer.
    pub fn new(config: &KafkaConfig) -> Result<Self> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("bootstrap.servers", &config.brokers)
            .set("group.id", &config.group_id)
            .set("auto.offset.reset", "latest")
            .set("enable.auto.commit", "true")
            // On a fresh local broker, the mesh topics may not exist yet.
            .set("allow.auto.create.topics", "true")
            .create()
            .context("failed to create Kafka consumer")?;

        info!(brokers = %config.brokers, group = %config.group_id, "Kafka consumer created");
        Ok(Self {
            consumer,
            brokers: config.brokers.clone(),
            topic_prefix: config.topic_prefix.clone(),
        })
    }

    /// Subscribe to all mesh topics and run the consumer loop.
    ///
    /// Deserialised messages are sent to `tx`. Runs until the sender is dropped
    /// or the consumer is shut down.
    pub async fn run(&self, tx: mpsc::Sender<MeshMessage>) -> Result<()> {
        let topics = mesh_topics(&self.topic_prefix);
        ensure_topics_exist(&self.brokers, &topics).await?;

        let topic_refs: Vec<&str> = topics.iter().map(|s| s.as_str()).collect();
        self.consumer
            .subscribe(&topic_refs)
            .context("failed to subscribe to mesh topics")?;

        info!(topics = ?topic_refs, "Kafka consumer subscribed");

        loop {
            match self.consumer.recv().await {
                Ok(borrowed_msg) => {
                    if let Some(payload) = borrowed_msg.payload_view::<str>().and_then(|r| r.ok()) {
                        match serde_json::from_str::<MeshMessage>(payload) {
                            Ok(msg) => {
                                if tx.send(msg).await.is_err() {
                                    debug!("mesh message channel closed, stopping consumer");
                                    break;
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "failed to deserialize mesh message from Kafka");
                            }
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("UnknownTopicOrPartition") {
                        debug!(error = %msg, "Kafka mesh topics not visible yet; retrying");
                    } else {
                        error!(error = %e, "Kafka consumer error");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }

        Ok(())
    }
}

fn mesh_topics(topic_prefix: &str) -> Vec<String> {
    [
        "announce",
        "goodbye",
        "data-publish",
        "data-subscribe",
        "data-unsubscribe",
        "query-forward",
        "query-result",
        "ping",
        "pong",
    ]
    .iter()
    .map(|suffix| format!("{topic_prefix}.{suffix}"))
    .collect()
}

async fn ensure_topics_exist(brokers: &str, topics: &[String]) -> Result<()> {
    let admin: AdminClient<DefaultClientContext> = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .create()
        .context("failed to create Kafka admin client")?;

    let new_topics: Vec<NewTopic<'_>> = topics
        .iter()
        .map(|topic| NewTopic::new(topic, 1, TopicReplication::Fixed(1)))
        .collect();

    // Fresh local brokers do not have the mesh topics yet. Creating them once
    // up front avoids the repeated UnknownTopicOrPartition error loop.
    let results = admin
        .create_topics(&new_topics, &AdminOptions::new())
        .await
        .context("failed to create mesh Kafka topics")?;

    for result in results {
        if let Err((topic, err)) = result {
            let msg = err.to_string();
            if !msg.contains("TopicAlreadyExists") {
                anyhow::bail!("failed to create Kafka topic {topic}: {msg}");
            }
        }
    }

    Ok(())
}
