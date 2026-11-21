use crate::domain::dates::Date;
use crate::domain::error::{Error, Result};
use crate::domain::indicators::IndicatorHistory;
use serde::Deserialize;
use std::path::Path;

/// Loads an `effective_date;rate_percent` history into `history` under `indicator`.
pub fn load_indicator_csv(
    history: &mut IndicatorHistory,
    indicator: &str,
    path: &Path,
) -> Result<()> {
    #[derive(Deserialize)]
    struct Row {
        effective_date: Date,
        rate_percent: f64,
    }
    let err = |e: csv::Error| Error::new(format!("{}: {e}", path.display()));
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .trim(csv::Trim::All)
        .from_path(path)
        .map_err(err)?;
    let mut points = Vec::new();
    for row in reader.deserialize::<Row>() {
        let row = row.map_err(err)?;
        points.push((row.effective_date, row.rate_percent / 100.0));
    }
    history.insert(indicator, points)
}
