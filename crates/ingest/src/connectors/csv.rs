use std::path::Path;

use anyhow::{Context, Result};
use polars::prelude::*;

use crate::DataSource;

/// Field separator implied by the file extension.
///
/// `.tsv` reached this connector through two routes that both advertise it
/// as supported — `IngestPipeline::ingest_file` maps `"csv" | "tsv"` to
/// `ingest_csv`, and the CLI's `ingest_backend` routes `tsv` to
/// `LocalTabular` — but the reader was built from `CsvReadOptions::default()`,
/// whose separator is a comma. A tab-separated file therefore parsed as ONE
/// column per row, and that single blob was what went to the LLM for entity
/// extraction. Nothing failed loudly: the ingest reported a successful
/// one-column schema.
fn separator_for(path: &Path) -> u8 {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("tsv") => b'\t',
        _ => b',',
    }
}

/// Delimited-text connector (CSV and TSV) for data ingestion.
pub struct CsvConnector;

impl CsvConnector {
    fn options(path: &Path) -> CsvReadOptions {
        CsvReadOptions::default()
            .with_parse_options(CsvParseOptions::default().with_separator(separator_for(path)))
    }

    /// Load a delimited file into a DataFrame, using the separator its
    /// extension implies.
    pub fn load(path: &Path) -> Result<DataFrame> {
        Self::options(path)
            .try_into_reader_with_file_path(Some(path.into()))
            .context("Failed to create CSV reader")?
            .finish()
            .context("Failed to read CSV file")
    }

    /// Load only the first `n_rows` from a delimited file.
    pub fn preview(path: &Path, n_rows: usize) -> Result<DataFrame> {
        Self::options(path)
            .with_n_rows(Some(n_rows))
            .try_into_reader_with_file_path(Some(path.into()))
            .context("Failed to create CSV reader")?
            .finish()
            .context("Failed to preview CSV file")
    }

    /// Convert a delimited file path into a DataSource descriptor.
    ///
    /// `format` reports the extension actually seen, so a `.tsv` ingest is
    /// not recorded — in `IngestResult` and in anything downstream that
    /// reads it — as having been a CSV.
    pub fn to_data_source(path: &Path) -> Result<DataSource> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("Path does not exist: {}", path.display()))?;
        let format = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("tsv") => "tsv",
            _ => "csv",
        };
        Ok(DataSource {
            path: canonical.to_string_lossy().into_owned(),
            format: format.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-test scratch directory, removed on drop.
    ///
    /// `tempfile` is a workspace dependency but not a dev-dependency of this
    /// crate, and adding it would mean touching the manifest and the lock for
    /// a fixture. The sibling tests in `pipeline.rs` already use
    /// `std::env::temp_dir()` + a uuid, so this follows that convention.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("prism_csv_test_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).expect("write fixture");
        p
    }

    /// The regression this file exists for: a tab-separated file must parse
    /// into its real columns, not one column holding the whole line.
    #[test]
    fn tsv_is_parsed_with_tabs_not_commas() {
        let dir = Scratch::new();
        let path = write(
            dir.path(),
            "alloys.tsv",
            "alloy\tuts_mpa\tphase\nTi-6Al-4V\t1140\talpha-beta\n",
        );

        let df = CsvConnector::load(&path).expect("load tsv");

        assert_eq!(
            df.width(),
            3,
            "tab-separated file collapsed into one column"
        );
        assert_eq!(df.height(), 1);
        assert_eq!(
            df.get_column_names()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            vec!["alloy", "uts_mpa", "phase"],
        );
    }

    /// The separator switch must not regress plain CSV.
    #[test]
    fn csv_still_parsed_with_commas() {
        let dir = Scratch::new();
        let path = write(
            dir.path(),
            "alloys.csv",
            "alloy,uts_mpa,phase\nTi-6Al-4V,1140,alpha-beta\n",
        );

        let df = CsvConnector::load(&path).expect("load csv");

        assert_eq!(df.width(), 3);
        assert_eq!(df.height(), 1);
    }

    /// A comma inside a TSV field is data, not a separator — the case that
    /// silently corrupted rows before the fix.
    #[test]
    fn commas_inside_tsv_fields_are_data() {
        let dir = Scratch::new();
        let path = write(
            dir.path(),
            "notes.tsv",
            "id\tnote\n1\tannealed, then quenched\n",
        );

        let df = CsvConnector::load(&path).expect("load tsv");

        assert_eq!(df.width(), 2, "the comma inside the note split the row");
    }

    #[test]
    fn preview_honours_the_tsv_separator_too() {
        let dir = Scratch::new();
        let path = write(dir.path(), "many.tsv", "a\tb\n1\t2\n3\t4\n5\t6\n");

        let df = CsvConnector::preview(&path, 2).expect("preview tsv");

        assert_eq!(df.width(), 2);
        assert_eq!(df.height(), 2);
    }

    #[test]
    fn data_source_format_reports_tsv_not_csv() {
        let dir = Scratch::new();
        let tsv = write(dir.path(), "x.tsv", "a\tb\n1\t2\n");
        let csv = write(dir.path(), "x.csv", "a,b\n1,2\n");

        assert_eq!(CsvConnector::to_data_source(&tsv).unwrap().format, "tsv");
        assert_eq!(CsvConnector::to_data_source(&csv).unwrap().format, "csv");
    }

    #[test]
    fn separator_is_chosen_case_insensitively() {
        assert_eq!(separator_for(Path::new("x.TSV")), b'\t');
        assert_eq!(separator_for(Path::new("x.tsv")), b'\t');
        assert_eq!(separator_for(Path::new("x.csv")), b',');
        assert_eq!(separator_for(Path::new("x")), b',');
    }
}
