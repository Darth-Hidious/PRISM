use anyhow::Result;
use polars::prelude::*;
use serde::{Deserialize, Serialize};

use crate::SchemaAnalysis;

/// Per-column profile produced during schema detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnProfile {
    pub name: String,
    pub detected_type: String,
    pub null_count: usize,
    pub null_pct: f64,
    pub unique_count: usize,
    pub sample_values: Vec<String>,
}

pub struct SchemaDetector;

impl SchemaDetector {
    /// Map a polars DataType to a human-readable string.
    fn dtype_to_string(dtype: &DataType) -> String {
        match dtype {
            DataType::Boolean => "boolean".into(),
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64
            | DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
                "integer".into()
            }
            DataType::Float32 | DataType::Float64 => "float".into(),
            DataType::String => "string".into(),
            DataType::Date => "date".into(),
            DataType::Datetime(_, _) => "datetime".into(),
            _ => "unknown".into(),
        }
    }

    /// Produce a SchemaAnalysis (column names + detected type strings) from a DataFrame.
    pub fn detect(df: &DataFrame) -> Result<SchemaAnalysis> {
        let columns: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
        let detected_types: Vec<String> = df
            .dtypes()
            .iter()
            .map(|dt| Self::dtype_to_string(dt))
            .collect();

        Ok(SchemaAnalysis {
            columns,
            detected_types,
        })
    }

    /// Produce detailed per-column profiles from a DataFrame.
    pub fn detect_column_profiles(df: &DataFrame) -> Result<Vec<ColumnProfile>> {
        let height = df.height();
        let mut profiles = Vec::with_capacity(df.width());

        for col in df.get_columns() {
            let name = col.name().to_string();
            let detected_type = Self::dtype_to_string(col.dtype());
            let null_count = col.null_count();
            let null_pct = if height == 0 {
                0.0
            } else {
                (null_count as f64 / height as f64) * 100.0
            };
            let unique_count = col.n_unique().unwrap_or(0);

            // Sample up to 5 non-null values.
            let sample_values: Vec<String> = (0..height.min(5))
                .filter_map(|i| {
                    let val = col.get(i).ok()?;
                    if val == AnyValue::Null {
                        None
                    } else {
                        Some(format!("{}", val))
                    }
                })
                .collect();

            profiles.push(ColumnProfile {
                name,
                detected_type,
                null_count,
                null_pct,
                unique_count,
                sample_values,
            });
        }

        Ok(profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_df() -> DataFrame {
        df!(
            "material" => &["Steel", "Copper", "Aluminum"],
            "density" => &[7.8, 8.96, 2.7],
            "melting_point" => &[1370, 1085, 660],
            "is_metal" => &[true, true, true]
        )
        .unwrap()
    }

    #[test]
    fn detect_schema_columns_and_types() {
        let df = sample_df();
        let schema = SchemaDetector::detect(&df).unwrap();

        assert_eq!(schema.columns, vec!["material", "density", "melting_point", "is_metal"]);
        assert_eq!(schema.detected_types, vec!["string", "float", "integer", "boolean"]);
    }

    #[test]
    fn detect_column_profiles_counts() {
        let df = sample_df();
        let profiles = SchemaDetector::detect_column_profiles(&df).unwrap();

        assert_eq!(profiles.len(), 4);
        for p in &profiles {
            assert_eq!(p.null_count, 0);
            assert!((p.null_pct - 0.0).abs() < f64::EPSILON);
            assert!(!p.sample_values.is_empty());
        }
        // material column should have 3 unique values
        assert_eq!(profiles[0].unique_count, 3);
    }

    #[test]
    fn detect_handles_nulls() {
        let s1 = Series::new("a".into(), &[Some(1), None, Some(3)]);
        let s2 = Series::new("b".into(), &[Some("x"), None, None]);
        let df = DataFrame::new(vec![s1.into(), s2.into()]).unwrap();

        let profiles = SchemaDetector::detect_column_profiles(&df).unwrap();
        assert_eq!(profiles[0].null_count, 1);
        assert_eq!(profiles[1].null_count, 2);
        assert!((profiles[1].null_pct - 66.666_666_666_666_66).abs() < 0.01);
    }

    #[test]
    fn detect_empty_dataframe() {
        let df = DataFrame::empty();
        let schema = SchemaDetector::detect(&df).unwrap();
        assert!(schema.columns.is_empty());
        assert!(schema.detected_types.is_empty());
    }
}
