use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub neo4j: Neo4jConfig,
    pub vector_db: VectorDbConfig,
    pub kafka: Option<KafkaConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Neo4jConfig {
    pub image: String,
    pub bolt_port: u16,
    pub http_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorDbConfig {
    pub image: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KafkaConfig {
    pub image: String,
    pub port: u16,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            neo4j: Neo4jConfig::default(),
            vector_db: VectorDbConfig::default(),
            kafka: None, // Kafka is optional — only needed for federated/air-gapped mode
        }
    }
}

impl Default for Neo4jConfig {
    fn default() -> Self {
        Self {
            image: "neo4j:5-community".to_string(),
            bolt_port: 7687,
            http_port: 7474,
        }
    }
}

impl Default for VectorDbConfig {
    fn default() -> Self {
        Self {
            image: "qdrant/qdrant:v1.12.6".to_string(),
            port: 6333,
        }
    }
}

impl Default for KafkaConfig {
    fn default() -> Self {
        Self {
            image: "apache/kafka:3.9".to_string(),
            port: 9092,
        }
    }
}
