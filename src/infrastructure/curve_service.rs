//! Client of the curve service (curve-parser): base G-curve and IRRBB scenarios for a date.
//!
//! `GET {base}/api/v1/curves/{date}?on_or_before=true` returns the snapshot of the last trading
//! day stored on or before that date: zero rates on the tenor grid (fractions, annual compounding) and, per tenor,
//! the rate of every shock scenario. The client turns it into one curve per scenario:
//! `BASE` and the scenario names in upper case (`PARALLEL_UP`, ...).

use crate::domain::error::{bail, Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// One scenario curve: zero rates in percent by tenor in days, annual compounding.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceCurve {
    pub scenario: String,
    pub points: Vec<(u32, f64)>,
}

#[derive(Clone, Debug)]
pub struct CurveService {
    base: String,
    agent: ureq::Agent,
}

#[derive(Deserialize)]
struct Snapshot {
    date: String,
    tenors: Vec<Tenor>,
}

#[derive(Deserialize)]
struct Tenor {
    tenor_years: f64,
    base_rate: f64,
    #[serde(default)]
    scenarios: Vec<ScenarioRate>,
}

#[derive(Deserialize)]
struct ScenarioRate {
    scenario: String,
    rate: f64,
}

#[derive(Deserialize)]
struct ServiceError {
    message: String,
}

impl CurveService {
    pub fn new(base: &str, timeout: Duration) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        Self { base: base.trim_end_matches('/').to_string(), agent }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Curves for `date` (`YYYY-MM-DD`) and the trading date they were measured on.
    pub fn curves(&self, date: &str) -> Result<(String, Vec<ServiceCurve>)> {
        // the last trading day on or before `date` (weekends and holidays have no curve)
        let url = format!("{}/api/v1/curves/{}?on_or_before=true", self.base, date);
        let snapshot: Snapshot = match self.agent.get(&url).call() {
            Ok(r) => r
                .into_json()
                .map_err(|e| Error::new(format!("curve service: unreadable snapshot for {date}: {e}")))?,
            Err(ureq::Error::Status(code, r)) => {
                let message = r
                    .into_json::<ServiceError>()
                    .map(|e| e.message)
                    .unwrap_or_else(|_| "no details".into());
                bail!("curve service answered {code} for {date}: {message}")
            }
            Err(e) => bail!("curve service is unreachable at {}: {e}", self.base),
        };
        Ok((snapshot.date.clone(), to_curves(&snapshot)?))
    }
}

fn to_curves(s: &Snapshot) -> Result<Vec<ServiceCurve>> {
    if s.tenors.is_empty() {
        bail!("curve service: snapshot {} has no tenors", s.date);
    }
    let mut by_scenario: BTreeMap<String, Vec<(u32, f64)>> = BTreeMap::new();
    let mut base = Vec::with_capacity(s.tenors.len());
    for t in &s.tenors {
        let days = ((t.tenor_years * 365.0).round() as u32).max(1);
        base.push((days, t.base_rate * 100.0));
        for sc in &t.scenarios {
            by_scenario
                .entry(sc.scenario.trim().to_ascii_uppercase())
                .or_default()
                .push((days, sc.rate * 100.0));
        }
    }
    let mut out = vec![ServiceCurve { scenario: "BASE".into(), points: base }];
    for (scenario, points) in by_scenario {
        if points.len() != s.tenors.len() {
            bail!("curve service: scenario {scenario} of {} misses tenors", s.date);
        }
        out.push(ServiceCurve { scenario, points });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_becomes_one_curve_per_scenario() {
        let s: Snapshot = serde_json::from_str(
            r#"{"date":"2023-02-28","tenors":[
                {"tenor_years":0.25,"base_rate":0.07,"scenarios":[{"scenario":"parallel_up","rate":0.11}]},
                {"tenor_years":1,"base_rate":0.08,"scenarios":[{"scenario":"parallel_up","rate":0.12}]}
            ]}"#,
        )
        .unwrap();
        let curves = to_curves(&s).unwrap();
        assert_eq!(curves.len(), 2);
        assert_eq!(curves[0].scenario, "BASE");
        assert_eq!(curves[0].points[0].0, 91);
        assert!((curves[0].points[1].1 - 8.0).abs() < 1e-12);
        assert_eq!(curves[1].scenario, "PARALLEL_UP");
        assert!((curves[1].points[0].1 - 11.0).abs() < 1e-12);
    }
}
