//! Behavioural prepayment: a constant prepayment rate (CPR) per behaviour model.
//!
//! A deal's `behavior_model_cd` names its model (`PREPAYMENT_MORTGAGE`, ...). The policy maps
//! model codes to an annual CPR; a code can also carry the rate itself (`CPR:0.12`, `CPR_12`).
//! Deals without a model, or with a model the policy does not know, keep their contractual
//! schedule.
//!
//! The contractual schedule is scaled by the share of principal that survives prepayment:
//! after `t` days from the report date that share is `S(t) = (1 - CPR)^(t / 365)`. On every
//! event of the schedule the client first pays the scheduled flows on the surviving balance,
//! then prepays the part of the remaining balance that leaves between this event and the
//! previous one. For annuities this is exactly "keep the term, lower the payment" (the
//! annuity is linear in the balance); for bullets it is a partial early repayment.

use crate::domain::dates::{Date, DateExt};
use crate::domain::error::{bail, Error, Result};
use crate::domain::model::{CashFlow, Deal};
use std::collections::BTreeMap;

/// Annual CPR by behaviour model code (upper case).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PrepaymentPolicy {
    rates: BTreeMap<String, f64>,
}

impl PrepaymentPolicy {
    /// Rates used when nothing else is configured. Conservative, scenario independent
    /// assumptions; replace them with the bank's calibrated values via `with_rates`.
    pub fn standard() -> Self {
        Self::default().with_rates([
            ("PREPAYMENT_MORTGAGE", 0.12),
            ("PREPAYMENT_CONSUMER", 0.20),
            ("PREPAYMENT_AUTO", 0.15),
            ("PREPAYMENT_MR8_CRED", 0.10),
            ("PREPAYMENT_MR8_DEP", 0.08),
        ])
    }

    /// Sets or replaces rates; codes are case insensitive.
    pub fn with_rates<K: AsRef<str>>(mut self, rates: impl IntoIterator<Item = (K, f64)>) -> Self {
        for (code, cpr) in rates {
            self.rates.insert(code.as_ref().trim().to_ascii_uppercase(), cpr);
        }
        self
    }

    /// Parses `CODE=CPR[,CODE=CPR]` on top of the standard rates (`CPR` is a fraction).
    pub fn parse(spec: &str) -> Result<Self> {
        let mut rates = Vec::new();
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let Some((code, value)) = item.split_once('=') else {
                bail!("prepayment rate must look like CODE=0.12, got {item:?}");
            };
            let cpr: f64 = value
                .trim()
                .parse()
                .map_err(|_| Error::new(format!("prepayment rate of {code} is not a number: {value:?}")))?;
            rates.push((code.to_string(), cpr));
        }
        let policy = Self::standard().with_rates(rates);
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<()> {
        for (code, cpr) in &self.rates {
            if !(0.0..1.0).contains(cpr) || !cpr.is_finite() {
                bail!("prepayment rate of {code} must be in [0, 1), got {cpr}");
            }
        }
        Ok(())
    }

    pub fn rates(&self) -> &BTreeMap<String, f64> {
        &self.rates
    }

    /// Annual CPR for a deal, or `None` when it keeps its contractual schedule.
    pub fn cpr(&self, deal: &Deal) -> Option<f64> {
        let code = deal.behavior_model.as_deref()?.trim().to_ascii_uppercase();
        let cpr = explicit_cpr(&code).or_else(|| self.rates.get(&code).copied())?;
        (cpr > 0.0).then_some(cpr)
    }
}

/// `CPR:0.12`, `CPR=0.12`, `CPR_12` or `CPR12` (a whole number is read as percent).
fn explicit_cpr(code: &str) -> Option<f64> {
    let rest = code.strip_prefix("CPR")?.trim_start_matches([':', '=', '_', ' ']);
    let v: f64 = rest.replace(',', ".").parse().ok()?;
    let v = if v >= 1.0 { v / 100.0 } else { v };
    (0.0..1.0).contains(&v).then_some(v)
}

/// Share of principal that has not been prepaid `days` after the report date.
#[inline]
pub fn survival(cpr: f64, days: i32) -> f64 {
    if days <= 0 {
        1.0
    } else {
        (1.0 - cpr).powf(days as f64 / 365.0)
    }
}

/// Rescales a contractual schedule (in date order) for prepayment at `cpr` from `report`.
pub fn apply_prepayment(flows: &mut [CashFlow], report: Date, cpr: f64) {
    let mut before = 1.0; // surviving share at the previous event
    for cf in flows.iter_mut() {
        let after = survival(cpr, report.days_until(cf.cashflow_date));
        let scheduled_after = cf.balance_after;
        cf.principal_cf *= before;
        cf.interest_cf *= before;
        cf.subsidy_cf *= before;
        cf.balance_before *= before;
        cf.prepayment_cf = scheduled_after * (before - after);
        cf.balance_after = scheduled_after * after;
        before = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> Date {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn flow(date: Date, principal: f64, interest: f64, before: f64) -> CashFlow {
        CashFlow {
            deal_id: 1,
            cashflow_date: date,
            currency: "RUB".into(),
            interest_period_start: date,
            interest_period_end: date,
            principal_cf: principal,
            interest_cf: interest,
            subsidy_cf: interest / 2.0,
            prepayment_cf: 0.0,
            balance_before: before,
            balance_after: before - principal,
        }
    }

    #[test]
    fn explicit_codes() {
        assert_eq!(explicit_cpr("CPR:0.12"), Some(0.12));
        assert_eq!(explicit_cpr("CPR_15"), Some(0.15));
        assert_eq!(explicit_cpr("CPR7.5"), Some(0.075));
        assert_eq!(explicit_cpr("PREPAYMENT_AUTO"), None);
    }

    #[test]
    fn principal_is_conserved_and_balances_chain() {
        let report = d(2022, 1, 1);
        let mut flows = vec![
            flow(d(2022, 7, 1), 250.0, 50.0, 1000.0),
            flow(d(2023, 1, 1), 250.0, 37.5, 750.0),
            flow(d(2023, 7, 1), 250.0, 25.0, 500.0),
            flow(d(2024, 1, 1), 250.0, 12.5, 250.0),
        ];
        apply_prepayment(&mut flows, report, 0.2);
        let repaid: f64 = flows.iter().map(|c| c.principal_cf + c.prepayment_cf).sum();
        assert!((repaid - 1000.0).abs() < 1e-9, "all principal comes back: {repaid}");
        for w in flows.windows(2) {
            assert!((w[0].balance_after - w[1].balance_before).abs() < 1e-9);
        }
        for c in &flows {
            let left = c.balance_before - c.principal_cf - c.prepayment_cf;
            assert!((left - c.balance_after).abs() < 1e-9);
        }
        assert_eq!(flows.last().unwrap().balance_after, 0.0);
        // a year in, 80% of the scheduled balance survives
        assert!((flows[1].balance_after - 500.0 * 0.8).abs() < 1e-6);
        // interest of the second period runs on the balance left after the first prepayment
        assert!((flows[1].interest_cf - 37.5 * flows[1].balance_before / 750.0).abs() < 1e-9);
    }

    #[test]
    fn policy_lookup() {
        let p = PrepaymentPolicy::parse("PREPAYMENT_AUTO=0.3").unwrap();
        assert_eq!(p.rates()["PREPAYMENT_AUTO"], 0.3);
        assert_eq!(p.rates()["PREPAYMENT_MORTGAGE"], 0.12);
        assert!(PrepaymentPolicy::parse("X=1.5").is_err());
        assert!(PrepaymentPolicy::parse("X").is_err());
    }
}
