//! cashflow-rs: IRRBB cash-flow calculator.
//!
//! Layers (dependencies point inwards):
//! - `domain`         — deal model, schedules, rates, curves, discounting (pure);
//! - `application`    — use cases and the ports they need;
//! - `infrastructure` — CSV/SQLite adapters;
//! - `interfaces`     — delivery: HTTP API (axum). The CLI lives in `src/main.rs`.

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod interfaces;

// Stable paths kept for existing callers.
pub use domain::{
    cashflow, curves, dates, discounting, error, indicators, model, principal, rates,
};
pub use infrastructure::csv_deals as parser;
pub use infrastructure::sqlite_curve_store as curve_store;

pub use domain::cashflow::{build_cashflows, CalcConfig};
pub use domain::error::{Error, Result};
pub use domain::model::{CashFlow, Deal, DiscountedCashFlow};
pub use domain::valuation::{calculate_deal, DealResult, DealSummary};
