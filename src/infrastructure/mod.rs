//! Adapters to the outside world: CSV files, SQLite storage.
//! They translate external formats into domain types and never hold business rules.

pub mod csv_curves;
pub mod csv_deals;
pub mod csv_indicators;
pub mod sqlite_curve_store;
