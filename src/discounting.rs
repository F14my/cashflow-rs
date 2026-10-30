use crate::curves::ZeroCurve;
use crate::dates::{Date, DateExt};
use crate::error::{bail, Result};
use crate::model::{CashFlow, DiscountedCashFlow};

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
    let discounted_total_cf = discounted_principal_cf + discounted_interest_cf;
    Ok(DiscountedCashFlow {
        cashflow,
        curve_date: curve.curve_date,
        discount_time,
        zero_rate,
        discount_factor,
        discounted_principal_cf,
        discounted_interest_cf,
        discounted_total_cf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::{Compounding, CurvePoint};
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
            balance_before: 100.0,
            balance_after: 0.0,
        };
        let got = discount_cashflow(cf, report, &curve).unwrap();
        assert!((got.discount_factor - 1.0 / 1.1).abs() < 1e-12);
        assert!((got.discounted_total_cf - 100.0).abs() < 1e-12);
    }
}
