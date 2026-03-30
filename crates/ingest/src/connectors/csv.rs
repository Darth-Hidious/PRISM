use std::path::Path;

use anyhow::{Context, Result};
use polars::prelude::*;

use crate::DataSource;

/// CSV file connector for data ingestion.
pub struct CsvConnector;

impl CsvConnector {
    /// Load a CSV file into a DataFrame.
    pub fn load(path: &Path) -> Result<DataFrame> {
        CsvReadOptions::default()
            .try_into_reader_with_file_path(Some(path.into()))
            .context("Failed to create CSV reader")?
            .finish()
            .context("Failed to read CSV file")
    }

    /// Load only the first `n_rows` from a CSV file.
    pub fn preview(path: &Path, n_rows: usize) -> Result<DataFrame> {
        CsvReadOptions::default()
            .with_n_rows(Some(n_rows))
            .try_into_reader_with_file_path(Some(path.into()))
            .context("Failed to create CSV reader")?
            .finish()
            .context("Failed to preview CSV file")
    }

    /// Convert a CSV file path into a DataSource descriptor.
    pub fn to_data_source(path: &Path) -> Result<DataSource> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("Path does not exist: {}", path.display()))?;
        Ok(DataSource {
            path: canonical.to_string_lossy().into_owned(),
            format: "csv".into(),
        })
    }
}
