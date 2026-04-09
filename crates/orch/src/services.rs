use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub neo4j: Option<Neo4jConfig>,
    pub vector_db: Option<VectorDbConfig>,
    pub kafka: Option<KafkaConfig>,
    pub spark: Option<SparkConfig>,
    pub firecrawl: Option<FirecrawlConfig>,
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
            image: "apache/kafka:latest".to_string(),
            port: 9092,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparkConfig {
    pub image: String,
    pub master_port: u16,
    pub ui_port: u16,
}

impl Default for SparkConfig {
    fn default() -> Self {
        Self {
            // Bitnami retired the old Spark tags; use the official Spark image line.
            image: "spark:3.5.8-scala2.12-java17-ubuntu".to_string(),
            master_port: 7077,
            ui_port: 8088,
        }
    }
}

/// Firecrawl — open-source web scraping & search engine.
/// Docker image: `ghcr.io/mendableai/firecrawl`
/// Default port: 3002 (API)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirecrawlConfig {
    pub image: String,
    pub port: u16,
}

impl Default for FirecrawlConfig {
    fn default() -> Self {
        Self {
            image: "ghcr.io/mendableai/firecrawl:latest".to_string(),
            port: 3002,
        }
    }
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            neo4j: Some(Neo4jConfig::default()),
            vector_db: Some(VectorDbConfig::default()),
            kafka: None,
            spark: None,
            firecrawl: Some(FirecrawlConfig::default()),
        }
    }
}
