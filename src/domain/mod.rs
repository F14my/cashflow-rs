//! Business core: deal model, schedules, rates, curves, discounting.
//! Pure calculations with no I/O and no knowledge of transport or storage.

pub mod cashflow;
pub mod curves;
pub mod dates;
pub mod discounting;
pub mod error;
pub mod indicators;
pub mod model;
pub mod prepayment;
pub mod principal;
pub mod rates;
pub mod roll;
pub mod valuation;
