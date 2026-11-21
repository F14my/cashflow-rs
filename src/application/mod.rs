//! Use cases. They orchestrate the domain through ports (traits) and are
//! shared by every delivery mechanism: CLI, HTTP.

pub mod portfolio;
pub mod ports;
pub mod valuation;
