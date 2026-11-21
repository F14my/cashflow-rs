//! Moving a deal to a later report date along its own contractual schedule.
//!
//! No new business and no behavioural prepayments: the outstanding principal on the
//! target date is the balance after the last scheduled payment on or before it.

use crate::domain::dates::Date;
use crate::domain::model::{CashFlow, Deal};

/// Balances below this are treated as fully repaid.
const REPAID: f64 = 0.005;

/// Outstanding principal after the last scheduled flow on or before `on`.
/// Flows must be in date order (as `build_cashflows` returns them).
pub fn balance_on(flows: &[CashFlow], outstanding: f64, on: Date) -> f64 {
    let idx = flows.partition_point(|c| c.cashflow_date <= on);
    if idx == 0 {
        outstanding
    } else {
        flows[idx - 1].balance_after
    }
}

/// State of `deal` on `to`, given its scheduled balance on that date.
/// Returns `None` when the deal has matured or is fully repaid by then.
pub fn roll_forward(deal: &Deal, balance: f64, to: Date) -> Option<Deal> {
    if deal.maturity_date <= to {
        return None;
    }
    if to <= deal.report_date {
        return Some(deal.clone());
    }
    if balance <= REPAID {
        return None;
    }
    let mut rolled = deal.clone();
    rolled.report_date = to;
    rolled.outstanding_principal = balance;
    rolled.accrued_interest = None;
    if !deal.principal_payments.is_empty() {
        rolled.principal_payments.retain(|p| p.date > to);
        if rolled.principal_payments.is_empty() {
            return None;
        }
        rolled.outstanding_principal = rolled.principal_payments.iter().map(|p| p.amount).sum();
    }
    rolled.interest_payment_dates.retain(|d| *d > to);
    Some(rolled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dates::{DayCount, Period, ScheduleRule};
    use crate::domain::model::{Discriminator, PrincipalPayment, RateSpec, RateType};
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> Date {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn deal() -> Deal {
        Deal {
            deal_id: 1,
            deal_date: d(2022, 1, 1),
            report_date: d(2022, 8, 31),
            maturity_date: d(2023, 8, 31),
            currency: "RUB".into(),
            discriminator: Discriminator::Simple,
            start_principal: None,
            outstanding_principal: 1000.0,
            accrued_interest: Some(5.0),
            day_count: DayCount::Act365,
            rate_spec: RateSpec {
                rate_type: RateType::Fixed,
                interest_rate: 0.1,
                indicator: None,
                indicator_rate: None,
                payment_rule: Some(ScheduleRule {
                    start_date: Some(d(2022, 1, 1)),
                    period: Some(Period::Months(1)),
                    day_num: None,
                }),
                repricing_rule: None,
                repricing_dates: vec![],
                rate_schedule: vec![],
                fixed_rate_schedule: vec![],
                cap: None,
                floor: None,
                coefficient: None,
                indicator_lag: None,
                first_repricing_event: None,
            },
            sub_rate_spec: None,
            principal_rule: None,
            interest_payment_dates: vec![d(2022, 9, 30), d(2022, 10, 31)],
            principal_payments: vec![],
            product: None,
            behavior_model: None,
        }
    }

    #[test]
    fn rolls_balance_and_drops_past_dates() {
        let r = roll_forward(&deal(), 600.0, d(2022, 9, 30)).unwrap();
        assert_eq!(r.report_date, d(2022, 9, 30));
        assert_eq!(r.outstanding_principal, 600.0);
        assert_eq!(r.accrued_interest, None);
        assert_eq!(r.interest_payment_dates, vec![d(2022, 10, 31)]);
    }

    #[test]
    fn matured_or_repaid_deals_disappear() {
        assert!(roll_forward(&deal(), 600.0, d(2023, 8, 31)).is_none());
        assert!(roll_forward(&deal(), 0.0, d(2022, 9, 30)).is_none());
    }

    #[test]
    fn explicit_principal_schedule_sets_the_balance() {
        let mut x = deal();
        x.principal_payments = vec![
            PrincipalPayment { date: d(2022, 9, 30), amount: 400.0 },
            PrincipalPayment { date: d(2022, 12, 31), amount: 600.0 },
        ];
        let r = roll_forward(&x, 999.0, d(2022, 10, 15)).unwrap();
        assert_eq!(r.outstanding_principal, 600.0);
        assert_eq!(r.principal_payments.len(), 1);
    }
}
