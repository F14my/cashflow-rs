use crate::dates::{Date, DayCount, Period};
use crate::error::Result;
use crate::indicators::IndicatorSource;
use crate::model::{CashFlow, Deal, RateSpec};
use crate::principal::build_principal_schedule;
use crate::rates::RateEngine;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default)]
pub struct CalcConfig {
    pub act_act_as_365: bool,
}

impl CalcConfig {
    pub fn day_count(&self, deal: &Deal) -> DayCount {
        match deal.day_count {
            DayCount::ActActIsda if self.act_act_as_365 => DayCount::Act365,
            dc => dc,
        }
    }
}

fn has_period(rule: &Option<crate::dates::ScheduleRule>) -> bool {
    matches!(
        rule.as_ref().and_then(|r| r.period),
        Some(Period::Months(_)) | Some(Period::Days(_)) | Some(Period::EndDate)
    )
}

fn previous_principal_date(deal: &Deal) -> Option<Date> {
    let explicit = deal
        .principal_payments
        .iter()
        .map(|p| p.date)
        .filter(|d| *d <= deal.report_date)
        .max();
    explicit.or_else(|| {
        deal.principal_rule
            .as_ref()
            .and_then(|r| r.last_on_or_before(deal.report_date))
    })
}

pub fn main_accrual_start(deal: &Deal) -> Date {
    let report = deal.report_date;
    let prev = if !deal.interest_payment_dates.is_empty() {
        return deal
            .interest_payment_dates
            .iter()
            .copied()
            .filter(|d| *d <= report)
            .max()
            .map(|d| d.max(deal.deal_date))
            .unwrap_or(report.max(deal.deal_date));
    } else if has_period(&deal.rate_spec.payment_rule) {
        deal.rate_spec
            .payment_rule
            .as_ref()
            .and_then(|r| r.last_on_or_before(report))
    } else {
        previous_principal_date(deal)
    };
    prev.map(|d| d.max(deal.deal_date))
        .unwrap_or(deal.deal_date)
}

pub fn sub_accrual_start(deal: &Deal, sub: &RateSpec) -> Date {
    let report = deal.report_date;
    let rule_start = sub.payment_rule.as_ref().and_then(|r| r.start_date);
    if let Some(s) = rule_start {
        if s > report {
            return s;
        }
    }
    let prev = if has_period(&sub.payment_rule) {
        sub.payment_rule
            .as_ref()
            .and_then(|r| r.last_on_or_before(report))
    } else {
        previous_principal_date(deal)
    };
    let floor = rule_start.unwrap_or(deal.deal_date).max(deal.deal_date);
    prev.map(|d| d.max(floor)).unwrap_or(floor)
}

fn interest_payment_dates(
    deal: &Deal,
    spec: &RateSpec,
    principal_dates: &[Date],
    explicit: &[Date],
) -> Result<Vec<Date>> {
    let (report, maturity) = (deal.report_date, deal.maturity_date);
    let mut dates: Vec<Date> = if !explicit.is_empty() {
        explicit
            .iter()
            .copied()
            .filter(|d| report < *d && *d <= maturity)
            .collect()
    } else if has_period(&spec.payment_rule) {
        spec.payment_rule
            .as_ref()
            .unwrap()
            .schedule(report, maturity, true)?
    } else {
        principal_dates.to_vec()
    };
    if maturity > report {
        dates.push(maturity);
    }
    dates.sort();
    dates.dedup();
    Ok(dates)
}

struct Leg<'a> {
    spec: &'a RateSpec,
    pay_dates: Vec<Date>,
    accrued_from: Date,
    accrued: f64,
    period_start: Date,
}

pub fn build_cashflows(
    deal: &Deal,
    indicators: &dyn IndicatorSource,
    cfg: &CalcConfig,
) -> Result<Vec<CashFlow>> {
    let rates = RateEngine::new(deal, indicators, cfg.day_count(deal));
    let principal = build_principal_schedule(deal, &rates)?;
    let mut principal_by_date: BTreeMap<Date, f64> = BTreeMap::new();
    for p in &principal {
        *principal_by_date.entry(p.date).or_insert(0.0) += p.amount;
    }
    let principal_dates: Vec<Date> = principal_by_date.keys().copied().collect();

    let (main_from, main_accrued) = match deal.accrued_interest {
        Some(a) => (deal.report_date, a),
        None => (main_accrual_start(deal), 0.0),
    };
    let mut legs = vec![Leg {
        spec: &deal.rate_spec,
        pay_dates: interest_payment_dates(
            deal,
            &deal.rate_spec,
            &principal_dates,
            &deal.interest_payment_dates,
        )?,
        accrued_from: main_from,
        accrued: main_accrued,
        period_start: main_from,
    }];
    if let Some(sub) = &deal.sub_rate_spec {
        let from = sub_accrual_start(deal, sub);
        legs.push(Leg {
            spec: sub,
            pay_dates: interest_payment_dates(deal, sub, &principal_dates, &[])?,
            accrued_from: from,
            accrued: 0.0,
            period_start: from,
        });
    }

    let mut events: Vec<Date> = principal_dates.clone();
    for leg in &legs {
        events.extend(leg.pay_dates.iter().copied());
    }
    if deal.maturity_date > deal.report_date {
        events.push(deal.maturity_date);
    }
    events.sort();
    events.dedup();

    let mut balance = deal.outstanding_principal;
    let mut out = Vec::with_capacity(events.len());

    for &event in &events {
        let mut interest_cf = 0.0;
        let mut paid_starts: Vec<Date> = Vec::new();
        for leg in legs.iter_mut() {
            if event > leg.accrued_from {
                leg.accrued += balance * rates.period_rate(leg.spec, leg.accrued_from, event)?;
                leg.accrued_from = event;
            }
            if leg.pay_dates.binary_search(&event).is_ok() {
                interest_cf += leg.accrued;
                leg.accrued = 0.0;
                paid_starts.push(leg.period_start);
                leg.period_start = event;
            }
        }
        let principal_cf = principal_by_date.get(&event).copied().unwrap_or(0.0);
        let balance_before = balance;
        let mut balance_after = balance_before - principal_cf;
        if balance_after.abs() < 1e-6 {
            balance_after = 0.0;
        }
        let (ps, pe) = match paid_starts.iter().min() {
            Some(s) => (*s, event),
            None => (event, event),
        };
        out.push(CashFlow {
            deal_id: deal.deal_id,
            cashflow_date: event,
            currency: deal.currency.clone(),
            interest_period_start: ps,
            interest_period_end: pe,
            principal_cf,
            interest_cf,
            balance_before,
            balance_after,
        });
        balance = balance_after;
    }
    Ok(out)
}
