use crate::cashflow::main_accrual_start;
use crate::dates::{Date, Period};
use crate::error::{bail, Result};
use crate::model::{Deal, Discriminator, PrincipalPayment};
use crate::rates::RateEngine;

const EPS: f64 = 1e-9;

fn direction(v: f64) -> f64 {
    if v < 0.0 {
        -1.0
    } else {
        1.0
    }
}

pub fn principal_payment_dates(deal: &Deal) -> Result<Vec<Date>> {
    match &deal.principal_rule {
        Some(rule) if rule.period.is_some() => {
            rule.schedule(deal.report_date, deal.maturity_date, true)
        }
        _ => Ok(if deal.maturity_date > deal.report_date {
            vec![deal.maturity_date]
        } else {
            vec![]
        }),
    }
}

pub fn build_principal_schedule(deal: &Deal, rates: &RateEngine) -> Result<Vec<PrincipalPayment>> {
    if deal.outstanding_principal == 0.0 {
        return Ok(vec![]);
    }
    if !deal.principal_payments.is_empty() {
        return reconcile_explicit_payments(deal);
    }
    match deal.discriminator {
        Discriminator::Simple => Ok(vec![PrincipalPayment {
            date: deal.maturity_date,
            amount: deal.outstanding_principal,
        }]),
        Discriminator::Rgm => {
            build_regular(deal.outstanding_principal, &principal_payment_dates(deal)?)
        }
        Discriminator::Ann => build_annuity(deal, rates),
    }
}

pub fn build_regular(principal: f64, dates: &[Date]) -> Result<Vec<PrincipalPayment>> {
    if dates.is_empty() {
        bail!("at least one payment date is required");
    }
    let dir = direction(principal);
    let mut remaining = principal.abs();
    let regular = remaining / dates.len() as f64;
    let mut out = Vec::with_capacity(dates.len());
    for (i, d) in dates.iter().enumerate() {
        let amount = if i + 1 == dates.len() {
            remaining
        } else {
            regular.min(remaining)
        };
        if amount > EPS {
            out.push(PrincipalPayment {
                date: *d,
                amount: amount * dir,
            });
        }
        remaining -= amount;
    }
    Ok(out)
}

pub fn annuity_payment(principal: f64, period_rate: f64, n: usize) -> f64 {
    if n == 0 {
        return principal;
    }
    if period_rate.abs() < 1e-15 {
        return principal / n as f64;
    }
    principal * period_rate / (1.0 - (1.0 + period_rate).powi(-(n as i32)))
}

pub fn build_annuity(deal: &Deal, rates: &RateEngine) -> Result<Vec<PrincipalPayment>> {
    let dates = principal_payment_dates(deal)?;
    if dates.is_empty() {
        bail!("at least one payment date is required");
    }
    let spec = &deal.rate_spec;
    let nominal = deal
        .principal_rule
        .as_ref()
        .and_then(|r| r.period)
        .and_then(Period::nominal_year_fraction)
        .unwrap_or(1.0 / 12.0);

    let (mut prev, mut carried_interest) = match deal.accrued_interest {
        Some(a) => (deal.report_date, a.abs()),
        None => {
            let from_rule = deal
                .principal_rule
                .as_ref()
                .and_then(|r| r.last_on_or_before(deal.report_date));
            let start = from_rule
                .map(|d| d.max(deal.deal_date))
                .unwrap_or_else(|| main_accrual_start(deal));
            (start, 0.0)
        }
    };

    let dir = direction(deal.outstanding_principal);
    let mut balance = deal.outstanding_principal.abs();
    let n = dates.len();
    let mut used_rate = rates.annual_rate_on(spec, prev)?;
    let mut payment = annuity_payment(balance, used_rate * nominal, n);
    let mut out = Vec::with_capacity(n);

    for (i, d) in dates.iter().enumerate() {
        let rate_now = rates.annual_rate_on(spec, prev)?;
        if i > 0 && (rate_now - used_rate).abs() > 1e-12 {
            used_rate = rate_now;
            payment = annuity_payment(balance, used_rate * nominal, n - i);
        }
        let interest = balance * rates.period_rate(spec, prev, *d)? + carried_interest;
        carried_interest = 0.0;
        let principal = if i + 1 == n {
            balance
        } else {
            (payment - interest).clamp(0.0, balance)
        };
        out.push(PrincipalPayment {
            date: *d,
            amount: principal * dir,
        });
        balance -= principal;
        prev = *d;
    }
    Ok(out)
}

pub fn reconcile_explicit_payments(deal: &Deal) -> Result<Vec<PrincipalPayment>> {
    let dir = direction(deal.outstanding_principal);
    let mut grouped: Vec<(Date, f64)> = Vec::new();
    for p in &deal.principal_payments {
        if p.date <= deal.report_date {
            continue;
        }
        if p.date > deal.maturity_date {
            bail!(
                "principal payment date {} is after maturity {}",
                p.date,
                deal.maturity_date
            );
        }
        let magnitude = p.amount * dir;
        if magnitude < 0.0 {
            bail!(
                "principal payment has a sign opposite to the balance on {}: {}",
                p.date,
                p.amount
            );
        }
        match grouped.iter_mut().find(|g| g.0 == p.date) {
            Some(g) => g.1 += magnitude,
            None => grouped.push((p.date, magnitude)),
        }
    }
    grouped.sort_by_key(|g| g.0);

    let mut remaining = deal.outstanding_principal.abs();
    let mut out: Vec<PrincipalPayment> = Vec::new();
    for (date, requested) in grouped {
        if remaining <= EPS {
            break;
        }
        let amount = requested.min(remaining);
        if amount > EPS {
            out.push(PrincipalPayment {
                date,
                amount: amount * dir,
            });
            remaining -= amount;
        }
    }
    if remaining > 1e-6 {
        match out.last_mut() {
            Some(last) if last.date == deal.maturity_date => last.amount += remaining * dir,
            _ => out.push(PrincipalPayment {
                date: deal.maturity_date,
                amount: remaining * dir,
            }),
        }
    }
    Ok(out)
}
