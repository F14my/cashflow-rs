use crate::domain::dates::Date;
use crate::domain::error::{bail, Error, Result};
use std::collections::HashMap;

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

    pub fn value_on(&self, indicator: &str, date: Date) -> Result<f64> {
        let s = self
            .series
            .get(indicator)
            .ok_or_else(|| Error::new(format!("unsupported indicator: {indicator:?}")))?;
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
