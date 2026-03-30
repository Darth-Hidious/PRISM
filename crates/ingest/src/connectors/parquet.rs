use std::path::Path;

use anyhow::{Context, Result};
use polars::prelude::*;

use crate::DataSource;

/// Parquet file connector for data ingestion.
pub struct ParquetConnector;

impl ParquetConnector {
    /// Load a Parquet file into a DataFrame.
    pub fn load(path: &Path) -> Result<DataFrame> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open parquet file: {}", path.display()))?;
        ParquetReader::new(file)
            .finish()
            .context("Failed to read Parquet file")
    }

    /// Convert a Parquet file path into a DataSource descriptor.
    pub fn to_data_source(path: &Path) -> Result<DataSource> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("Path does not exist: {}", path.display()))?;
        Ok(DataSource {
            path: canonical.to_string_lossy().into_owned(),
            format: "parquet".into(),
        })
    }
}
