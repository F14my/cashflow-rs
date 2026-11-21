use crate::domain::curves::ZeroCurve;
use crate::domain::dates::{Date, DateExt};
use crate::domain::error::{bail, Result};
use crate::domain::model::{CashFlow, DiscountedCashFlow};

pub fn discount_cashflow(
    cashflow: CashFlow,
    report_date: Date,
    curve: &ZeroCurve,
) -> Result<DiscountedCashFlow> {
    if cashflow.cashflow_date < report_date {
        bail!(
            "cash flow date {} is before report date {}",
            cashflow.cashflow_date,
            report_date
        );
    }
    let tenor_days = report_date.days_until(cashflow.cashflow_date) as u32;
    let discount_time = tenor_days as f64 / 365.0;
    let zero_rate = curve.zero_rate(tenor_days.max(1));
    let discount_factor = if tenor_days == 0 {
        1.0
    } else {
        curve.discount_factor(discount_time, zero_rate)?
    };
    let discounted_principal_cf = cashflow.principal_cf * discount_factor;
    let discounted_interest_cf = cashflow.interest_cf * discount_factor;
    let discounted_subsidy_cf = cashflow.subsidy_cf * discount_factor;
    let discounted_prepayment_cf = cashflow.prepayment_cf * discount_factor;
    let discounted_total_cf =
        discounted_principal_cf + discounted_prepayment_cf + discounted_interest_cf + discounted_subsidy_cf;
    Ok(DiscountedCashFlow {
        cashflow,
        curve_date: curve.curve_date,
        discount_time,
        zero_rate,
        discount_factor,
        discounted_principal_cf,
        discounted_interest_cf,
        discounted_subsidy_cf,
        discounted_prepayment_cf,
        discounted_total_cf,
    })
}

/// Discount factors of one curve by whole days from the valuation date.
///
/// Discounting depends only on the day offset (`days / 365`), so a portfolio valued on one
/// date can look factors up instead of interpolating and exponentiating per cash flow.
/// Gives exactly the same numbers as [`discount_cashflow`].
pub struct DiscountTable {
    curve: ZeroCurve,
    factors: Vec<f64>,
}

impl DiscountTable {
    pub fn new(curve: ZeroCurve, max_days: u32) -> Result<Self> {
        let mut factors = Vec::with_capacity(max_days as usize + 1);
        for days in 0..=max_days {
            factors.push(Self::compute(&curve, days)?);
        }
        Ok(Self { curve, factors })
    }

    fn compute(curve: &ZeroCurve, days: u32) -> Result<f64> {
        if days == 0 {
            return Ok(1.0);
        }
        curve.discount_factor(days as f64 / 365.0, curve.zero_rate(days))
    }

    /// Factor for a cash flow `days` after the valuation date.
    #[inline]
    pub fn factor(&self, days: u32) -> Result<f64> {
        match self.factors.get(days as usize) {
            Some(f) => Ok(*f),
            None => Self::compute(&self.curve, days),
        }
    }

    pub fn scenario(&self) -> &str {
        &self.curve.scenario
    }

    pub fn currency(&self) -> &str {
        &self.curve.currency
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::curves::{Compounding, CurvePoint};
    use chrono::NaiveDate;

    #[test]
    fn discounts_principal_and_interest_separately() {
        let report = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let curve = ZeroCurve::new(
            report,
            "RUB",
            vec![CurvePoint {
                tenor_days: 365,
                zero_rate: 0.10,
            }],
            Compounding::Annual,
        )
        .unwrap();
        let cf = CashFlow {
            deal_id: 1,
            cashflow_date: NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
            currency: "RUB".into(),
            interest_period_start: report,
            interest_period_end: NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
            principal_cf: 100.0,
            interest_cf: 10.0,
            subsidy_cf: 0.0,
            prepayment_cf: 0.0,
            balance_before: 100.0,
            balance_after: 0.0,
        };
        let got = discount_cashflow(cf, report, &curve).unwrap();
        assert!((got.discount_factor - 1.0 / 1.1).abs() < 1e-12);
        assert!((got.discounted_total_cf - 100.0).abs() < 1e-12);
    }

    #[test]
    fn table_matches_direct_discounting() {
        let report = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let curve = ZeroCurve::new(
            report,
            "RUB",
            vec![
                CurvePoint { tenor_days: 30, zero_rate: 0.08 },
                CurvePoint { tenor_days: 3650, zero_rate: 0.11 },
            ],
            Compounding::Annual,
        )
        .unwrap();
        let table = DiscountTable::new(curve.clone(), 400).unwrap();
        for days in [0u32, 1, 17, 365, 399, 400, 5000] {
            let cf = CashFlow {
                deal_id: 1,
                cashflow_date: report + chrono::Days::new(days as u64),
                currency: "RUB".into(),
                interest_period_start: report,
                interest_period_end: report,
                principal_cf: 1.0,
                interest_cf: 0.0,
                subsidy_cf: 0.0,
                prepayment_cf: 0.0,
                balance_before: 0.0,
                balance_after: 0.0,
            };
            let direct = discount_cashflow(cf, report, &curve).unwrap().discount_factor;
            assert_eq!(table.factor(days).unwrap(), direct, "days={days}");
        }
    }
}
