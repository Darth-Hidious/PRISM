use polars::prelude::*;
use serde::{Deserialize, Serialize};

/// Severity level for a validation issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A single validation issue found during data quality checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub severity: Severity,
    pub column: Option<String>,
    pub message: String,
}

/// Summary of all validation checks on a DataFrame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
    pub passed: bool,
}

/// Run basic data quality checks on a DataFrame.
///
/// Checks performed:
/// - Empty DataFrame (Error)
/// - Columns with >50% null values (Warning)
/// - Duplicate column names (Error)
/// - Zero-variance numeric columns (Info)
pub fn validate(df: &DataFrame) -> ValidationReport {
    let mut issues = Vec::new();

    // 1. Empty DataFrame
    if df.height() == 0 {
        issues.push(ValidationIssue {
            severity: Severity::Error,
            column: None,
            message: "DataFrame is empty (0 rows)".into(),
        });
    }

    let height = df.height();

    // 2. Duplicate column names
    {
        let names: Vec<String> = df.get_column_names().iter().map(|s| s.to_string()).collect();
        let mut seen = std::collections::HashSet::new();
        for name in &names {
            if !seen.insert(name.clone()) {
                issues.push(ValidationIssue {
                    severity: Severity::Error,
                    column: Some(name.clone()),
                    message: format!("Duplicate column name: '{}'", name),
                });
            }
        }
    }

    if height > 0 {
        for col in df.get_columns() {
            let col_name = col.name().to_string();

            // 3. >50% nulls
            let null_count = col.null_count();
            let null_pct = null_count as f64 / height as f64;
            if null_pct > 0.5 {
                issues.push(ValidationIssue {
                    severity: Severity::Warning,
                    column: Some(col_name.clone()),
                    message: format!(
                        "Column '{}' has {:.1}% null values ({}/{})",
                        col_name,
                        null_pct * 100.0,
                        null_count,
                        height
                    ),
                });
            }

            // 4. Zero-variance numeric columns
            if matches!(
                col.dtype(),
                DataType::Float32
                    | DataType::Float64
                    | DataType::Int8
                    | DataType::Int16
                    | DataType::Int32
                    | DataType::Int64
                    | DataType::UInt8
                    | DataType::UInt16
                    | DataType::UInt32
                    | DataType::UInt64
            ) {
                if let Ok(n_unique) = col.n_unique() {
                    // n_unique counts null as a unique value, so adjust
                    let non_null_unique = if col.null_count() > 0 {
                        n_unique.saturating_sub(1)
                    } else {
                        n_unique
                    };
                    if non_null_unique <= 1 && height > 1 {
                        issues.push(ValidationIssue {
                            severity: Severity::Info,
                            column: Some(col_name.clone()),
                            message: format!(
                                "Column '{}' has zero variance (all non-null values are identical)",
                                col_name
                            ),
                        });
                    }
                }
            }
        }
    }

    let has_errors = issues.iter().any(|i| i.severity == Severity::Error);
    ValidationReport {
        passed: !has_errors,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_df_fails_validation() {
        let df = DataFrame::empty();
        let report = validate(&df);
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.severity == Severity::Error
            && i.message.contains("empty")));
    }

    #[test]
    fn clean_df_passes() {
        let df = df!(
            "a" => &[1, 2, 3],
            "b" => &[4.0, 5.0, 6.0]
        )
        .unwrap();
        let report = validate(&df);
        assert!(report.passed);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn high_null_column_warns() {
        let s = Series::new("mostly_null".into(), &[Option::<i32>::None, None, Some(1), None]);
        let df = DataFrame::new(vec![s.into()]).unwrap();
        let report = validate(&df);
        // 75% nulls > 50% threshold
        assert!(report.issues.iter().any(|i| i.severity == Severity::Warning
            && i.column.as_deref() == Some("mostly_null")));
    }

    #[test]
    fn zero_variance_column_info() {
        let df = df!(
            "constant" => &[42, 42, 42, 42],
            "varying" => &[1, 2, 3, 4]
        )
        .unwrap();
        let report = validate(&df);
        assert!(report.passed); // Info doesn't fail
        assert!(report.issues.iter().any(|i| i.severity == Severity::Info
            && i.column.as_deref() == Some("constant")));
        // varying column should NOT appear as zero-variance
        assert!(!report.issues.iter().any(|i| i.column.as_deref() == Some("varying")));
    }

    #[test]
    fn exactly_50pct_nulls_does_not_warn() {
        let s = Series::new("half_null".into(), &[Some(1), None]);
        let df = DataFrame::new(vec![s.into()]).unwrap();
        let report = validate(&df);
        // 50% is not >50%, so no warning
        assert!(!report.issues.iter().any(|i| i.severity == Severity::Warning));
    }
}
