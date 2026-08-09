//! File connectors: one module per format, dispatched through the registry.
//!
//! [`connector`] owns the [`Connector`] trait and [`ConnectorRegistry`] — the
//! one place extensions are mapped to connectors. Format modules implement
//! the trait and are registered in [`ConnectorRegistry::builtin`] (built-ins)
//! or at runtime through [`register_connector`]; a built-in is deliberately
//! swapped out through [`replace_connector`].

pub mod connector;
pub mod csv;
pub mod parquet;

pub use self::connector::{
    Connector, ConnectorRegistry, register_connector, registry, replace_connector,
};
pub use self::csv::CsvConnector;
pub use self::parquet::ParquetConnector;
