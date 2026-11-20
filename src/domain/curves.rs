use crate::domain::dates::Date;
use crate::domain::error::{bail, Error, Result};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compounding {
    Continuous,
    Annual,
}

impl Compounding {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "continuous" | "cont" => Ok(Self::Continuous),
            "annual" => Ok(Self::Annual),
            other => bail!("unsupported discount compounding: {other:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    pub tenor_days: u32,
    pub zero_rate: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ZeroCurve {
    pub curve_type: String,
    pub curve_date: Date,
    pub currency: String,
    pub scenario: String,
    pub points: Vec<CurvePoint>,
    pub compounding: Compounding,
}

impl ZeroCurve {
    pub fn new(
        curve_date: Date,
        currency: impl Into<String>,
        points: Vec<CurvePoint>,
        compounding: Compounding,
    ) -> Result<Self> {
        Self::new_named("G_CURVE", curve_date, currency, "BASE", points, compounding)
    }

    pub fn new_named(
        curve_type: impl Into<String>,
        curve_date: Date,
        currency: impl Into<String>,
        scenario: impl Into<String>,
        mut points: Vec<CurvePoint>,
        compounding: Compounding,
    ) -> Result<Self> {
        let curve_type = curve_type.into().trim().to_ascii_uppercase();
        let currency = currency.into().trim().to_ascii_uppercase();
        let scenario = scenario.into().trim().to_ascii_uppercase();
        if curve_type.is_empty() || currency.is_empty() || scenario.is_empty() {
            bail!("curve type, currency and scenario must not be empty");
        }
        if points.is_empty() {
            bail!("discount curve {currency} on {curve_date} has no points");
        }
        if points
            .iter()
            .any(|p| p.tenor_days == 0 || !p.zero_rate.is_finite())
        {
            bail!("discount curve {currency} on {curve_date} has an invalid point");
        }
        points.sort_by_key(|p| p.tenor_days);
        if points
            .windows(2)
            .any(|w| w[0].tenor_days == w[1].tenor_days)
        {
            bail!("discount curve {currency} on {curve_date} has duplicate tenors");
        }
        Ok(Self {
            curve_type,
            curve_date,
            currency,
            scenario,
            points,
            compounding,
        })
    }

    pub fn zero_rate(&self, tenor_days: u32) -> f64 {
        let idx = self.points.partition_point(|p| p.tenor_days < tenor_days);
        if idx == 0 {
            return self.points[0].zero_rate;
        }
        if idx == self.points.len() {
            return self.points[self.points.len() - 1].zero_rate;
        }
        let left = self.points[idx - 1];
        let right = self.points[idx];
        let weight =
            (tenor_days - left.tenor_days) as f64 / (right.tenor_days - left.tenor_days) as f64;
        left.zero_rate + weight * (right.zero_rate - left.zero_rate)
    }

    pub fn discount_factor(&self, time: f64, rate: f64) -> Result<f64> {
        if !time.is_finite() || time < 0.0 || !rate.is_finite() {
            bail!("invalid discount arguments: time={time}, rate={rate}");
        }
        let factor = match self.compounding {
            Compounding::Continuous => (-rate * time).exp(),
            Compounding::Annual => {
                if rate <= -1.0 {
                    bail!("annual zero rate must be greater than -100%: {rate}");
                }
                (1.0 + rate).powf(-time)
            }
        };
        Ok(factor)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CurveSet {
    curves: HashMap<(String, String, String), BTreeMap<Date, ZeroCurve>>,
}

pub trait CurveProvider {
    fn get_curve(
        &self,
        curve_type: &str,
        currency: &str,
        report_date: Date,
        scenario: &str,
    ) -> Result<ZeroCurve>;
}

impl CurveSet {
    pub fn curve_on(&self, currency: &str, report_date: Date) -> Result<&ZeroCurve> {
        self.curve_for("G_CURVE", currency, report_date, "BASE")
    }

    pub fn curve_for(
        &self,
        curve_type: &str,
        currency: &str,
        report_date: Date,
        scenario: &str,
    ) -> Result<&ZeroCurve> {
        let curve_type = curve_type.trim().to_ascii_uppercase();
        let code = currency.trim().to_ascii_uppercase();
        let scenario = scenario.trim().to_ascii_uppercase();
        let by_date = self
            .curves
            .get(&(curve_type.clone(), code.clone(), scenario.clone()))
            .ok_or_else(|| Error::new(format!("discount curve is unavailable for type={curve_type:?}, currency={code:?}, scenario={scenario:?}")))?;
        by_date
            .range(..=report_date)
            .next_back()
            .map(|(_, curve)| curve)
            .ok_or_else(|| {
                Error::new(format!(
                    "discount curve for {code} is unavailable on or before {report_date}"
                ))
            })
    }

    pub fn from_curves(curves: Vec<ZeroCurve>) -> Result<Self> {
        let mut set = Self::default();
        for curve in curves {
            let key = (
                curve.curve_type.clone(),
                curve.currency.clone(),
                curve.scenario.clone(),
            );
            if set
                .curves
                .entry(key)
                .or_default()
                .insert(curve.curve_date, curve)
                .is_some()
            {
                bail!("duplicate curve identity");
            }
        }
        Ok(set)
    }

    pub fn curves(&self) -> impl Iterator<Item = &ZeroCurve> {
        self.curves.values().flat_map(|by_date| by_date.values())
    }
}

impl CurveProvider for CurveSet {
    fn get_curve(
        &self,
        curve_type: &str,
        currency: &str,
        report_date: Date,
        scenario: &str,
    ) -> Result<ZeroCurve> {
        self.curve_for(curve_type, currency, report_date, scenario)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn curve(compounding: Compounding) -> ZeroCurve {
        ZeroCurve::new(
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            "RUB",
            vec![
                CurvePoint {
                    tenor_days: 30,
                    zero_rate: 0.10,
                },
                CurvePoint {
                    tenor_days: 90,
                    zero_rate: 0.16,
                },
            ],
            compounding,
        )
        .unwrap()
    }

    #[test]
    fn interpolates_and_flattens_curve_ends() {
        let c = curve(Compounding::Continuous);
        assert!((c.zero_rate(60) - 0.13).abs() < 1e-12);
        assert!((c.zero_rate(1) - 0.10).abs() < 1e-12);
        assert!((c.zero_rate(365) - 0.16).abs() < 1e-12);
    }

    #[test]
    fn discount_factor_respects_compounding() {
        let continuous = curve(Compounding::Continuous);
        assert!((continuous.discount_factor(0.0, 0.10).unwrap() - 1.0).abs() < 1e-12);
        assert!((continuous.discount_factor(1.0, 0.10).unwrap() - (-0.1_f64).exp()).abs() < 1e-12);
        let annual = curve(Compounding::Annual);
        assert!((annual.discount_factor(1.0, 0.10).unwrap() - 1.0 / 1.1).abs() < 1e-12);
    }
}
