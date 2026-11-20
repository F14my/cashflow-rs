use crate::domain::curves::{Compounding, CurvePoint, CurveSet, ZeroCurve};
use crate::domain::dates::Date;
use crate::domain::error::{bail, Error, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

fn default_curve_type() -> String {
    "G_CURVE".into()
}

fn default_scenario() -> String {
    "BASE".into()
}

/// Loads zero curves from `curve_type;curve_date;currency;scenario;tenor_days;zero_rate_percent`.
/// `curve_type` and `scenario` are optional columns.
pub fn load_curve_csv(path: &Path, compounding: Compounding) -> Result<CurveSet> {
    #[derive(Deserialize)]
    struct Row {
        #[serde(default = "default_curve_type")]
        curve_type: String,
        curve_date: Date,
        currency: String,
        #[serde(default = "default_scenario")]
        scenario: String,
        tenor_days: u32,
        zero_rate_percent: f64,
    }

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .trim(csv::Trim::All)
        .from_path(path)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
    let mut grouped: HashMap<(String, String, String, Date), Vec<CurvePoint>> = HashMap::new();
    for row in reader.deserialize::<Row>() {
        let row = row.map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        if !row.zero_rate_percent.is_finite() {
            bail!("{}: non-finite zero rate", path.display());
        }
        grouped
            .entry((
                row.curve_type.trim().to_ascii_uppercase(),
                row.currency.trim().to_ascii_uppercase(),
                row.scenario.trim().to_ascii_uppercase(),
                row.curve_date,
            ))
            .or_default()
            .push(CurvePoint {
                tenor_days: row.tenor_days,
                zero_rate: row.zero_rate_percent / 100.0,
            });
    }
    if grouped.is_empty() {
        bail!("{}: no discount curves", path.display());
    }
    let curves = grouped
        .into_iter()
        .map(|((curve_type, currency, scenario, date), points)| {
            ZeroCurve::new_named(&curve_type, date, &currency, &scenario, points, compounding)
        })
        .collect::<Result<Vec<_>>>()?;
    CurveSet::from_curves(curves)
}
