//! MARC27 billing / credits API client.
//!
//! Mirrors the platform's `/billing/*` namespace. The status bar polls the
//! org credit balance cheaply at turn boundaries.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/billing/balance` | Current org credit balance |

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api::PlatformClient;

/// Org credit balance, as returned by `GET /billing/balance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    /// Balance in millicredits (1000 millicredits = 1 credit). The
    /// authoritative integer value used for display.
    #[serde(default)]
    pub balance_millicredits: i64,
    /// Convenience float (`balance_millicredits / 1000`).
    #[serde(default)]
    pub credits: f64,
    /// Approximate USD value of the balance.
    #[serde(default)]
    pub dollar_value: f64,
    /// Owning organisation name.
    #[serde(default)]
    pub org_name: String,
}

/// Fetch the authenticated org's current credit balance.
///
/// The caller supplies a `PlatformClient` already carrying the base URL
/// (`…/api/v1`) and bearer token, so the path is just `/billing/balance`.
pub async fn get_balance(platform: &PlatformClient) -> Result<Balance> {
    platform
        .get("/billing/balance")
        .await
        .context("billing balance fetch failed")
}

/// Format a millicredit balance for the status bar, e.g. `91_015 → "91.0 cr"`.
#[must_use]
pub fn format_credits(millicredits: i64) -> String {
    format!("{:.1} cr", millicredits as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_millicredits_to_one_decimal() {
        assert_eq!(format_credits(91_015), "91.0 cr");
        assert_eq!(format_credits(0), "0.0 cr");
        assert_eq!(format_credits(1_500), "1.5 cr");
        assert_eq!(format_credits(250), "0.2 cr");
    }

    #[test]
    fn balance_deserializes_from_platform_shape() {
        let b: Balance = serde_json::from_str(
            r#"{"balance_millicredits":91015,"credits":91.015,"dollar_value":0.91015,"org_name":"MARC27 Open"}"#,
        )
        .unwrap();
        assert_eq!(b.balance_millicredits, 91_015);
        assert_eq!(format_credits(b.balance_millicredits), "91.0 cr");
        assert_eq!(b.org_name, "MARC27 Open");
    }
}
