use crate::dates::Date;
use crate::error::{bail, Error, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub trait IndicatorSource: Sync {
    fn rate(&self, indicator: &str, fixing_date: Date, report_date: Date) -> Result<f64>;
}

#[derive(Default, Clone, Debug)]
pub struct IndicatorHistory {
    series: HashMap<String, Vec<(Date, f64)>>,
}

impl IndicatorHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, indicator: &str, mut points: Vec<(Date, f64)>) -> Result<()> {
        points.sort_by_key(|p| p.0);
        if points.windows(2).any(|w| w[0].0 == w[1].0) {
            bail!("indicator {indicator}: duplicate dates");
        }
        if points.is_empty() {
            bail!("indicator {indicator}: empty history");
        }
        self.series.insert(indicator.to_string(), points);
        Ok(())
    }

    pub fn load_percent_csv(&mut self, indicator: &str, path: &Path) -> Result<()> {
        #[derive(Deserialize)]
        struct Row {
            effective_date: Date,
            rate_percent: f64,
        }
        let err = |e: csv::Error| Error::new(format!("{}: {e}", path.display()));
        let mut reader = csv::ReaderBuilder::new().delimiter(b';').trim(csv::Trim::All).from_path(path).map_err(err)?;
        let mut points = Vec::new();
        for row in reader.deserialize::<Row>() {
            let row = row.map_err(err)?;
            points.push((row.effective_date, row.rate_percent / 100.0));
        }
        self.insert(indicator, points)
    }

    pub fn value_on(&self, indicator: &str, date: Date) -> Result<f64> {
        let s = self.series.get(indicator).ok_or_else(|| Error::new(format!("unsupported indicator: {indicator:?}")))?;
        let idx = s.partition_point(|p| p.0 <= date);
        if idx == 0 {
            bail!("{indicator} is unavailable on {date}");
        }
        Ok(s[idx - 1].1)
    }
}

impl IndicatorSource for IndicatorHistory {
    fn rate(&self, indicator: &str, fixing_date: Date, report_date: Date) -> Result<f64> {
        self.value_on(indicator, fixing_date.min(report_date))
    }
}