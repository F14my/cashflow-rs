use crate::dates::{parse_date, Date, DayCount, Period, ScheduleRule};
use crate::error::{bail, Error, Result};
use crate::model::*;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct DealRow {
    report_dttm: String,
    discriminator: String,
    deal_id: i64,
    currency_id: String,
    start_principal_amt: Option<f64>,
    current_principal_amt: f64,
    deal_dttm: String,
    maturity_dttm: String,
    rate_type_cd: String,
    interest_rate: f64,
    base_indicator_rate: Option<f64>,
    base_indicator: Option<String>,
    interest_payment_rule_start_dttm: Option<String>,
    interest_payment_rule_period_cd: Option<String>,
    interest_payment_rule_day_num: Option<i32>,
    day_convention_cd: String,
    regular_payment_rule_start_dttm: Option<String>,
    regular_payment_rule_period_cd: Option<String>,
    regular_payment_rule_day_num: Option<i32>,
    repricing_rule_start_dttm: Option<String>,
    repricing_rule_period_cd: Option<String>,
    repricing_rule_day_num: Option<i32>,
    accrued_interest: Option<f64>,
    product: Option<String>,
    behavior_model_cd: Option<String>,
    interest_payment: Option<String>,
    principal_payment: Option<String>,
    repricing_dttm_schedule: Option<String>,
    repricing_dttm_rate_schedule: Option<String>,
    repricing_dttm_rate_schedule_fixed: Option<String>,
    cap_limit_interest_rate: Option<f64>,
    floor_limit_interest_rate: Option<f64>,
    sub_rate_type_cd: Option<String>,
    sub_interest_rate: Option<f64>,
    sub_float_indicator: Option<String>,
    sub_payment_rule_period_cd: Option<String>,
    sub_payment_rule_day_num: Option<i32>,
    sub_payment_rule_start_dttm: Option<String>,
    sub_repricing_rule_start_dttm: Option<String>,
    sub_repricing_rule_period_cd: Option<String>,
    sub_repricing_rule_day_num: Option<i32>,
    sub_cap_limit_interest_rate: Option<f64>,
    sub_floor_limit_interest_rate: Option<f64>,
    sub_coeff_index: Option<f64>,
    deal_report_dttm: Option<String>,
    sub_repricing_rule_first_event_flg: Option<u8>,
    indicator_lag_cd: Option<String>,
    sub_indicator_lag_cd: Option<String>,
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn opt<T>(v: &Option<String>, col: &str, f: impl Fn(&str) -> Result<T>) -> Result<Option<T>> {
    non_empty(v)
        .map(|s| f(s).map_err(|e| e.context(format!("column {col:?}"))))
        .transpose()
}

fn req_date(v: &str, col: &str) -> Result<Date> {
    parse_date(v).map_err(|e| e.context(format!("column {col:?}")))
}

fn date_list(s: &str) -> Result<Vec<Date>> {
    s.split_whitespace().map(parse_date).collect()
}

fn dated_pairs(s: &str) -> Result<Vec<(Date, f64)>> {
    s.split_whitespace()
        .map(|item| {
            let (d, v) = item
                .split_once(',')
                .ok_or_else(|| Error::new(format!("invalid dated value: {item:?}")))?;
            let v: f64 = v
                .parse()
                .map_err(|_| Error::new(format!("not a number: {v:?}")))?;
            Ok((parse_date(d)?, v))
        })
        .collect()
}

fn dated_rates(v: &Option<String>, col: &str) -> Result<Vec<DatedRate>> {
    Ok(opt(v, col, dated_pairs)?
        .unwrap_or_default()
        .into_iter()
        .map(|(date, rate)| DatedRate { date, rate })
        .collect())
}

fn schedule_rule(
    start: &Option<String>,
    period: &Option<String>,
    day: Option<i32>,
    prefix: &str,
) -> Result<Option<ScheduleRule>> {
    let start_date = opt(start, &format!("{prefix}_start_dttm"), parse_date)?;
    let period = opt(period, &format!("{prefix}_period_cd"), Period::parse)?;
    if start_date.is_none() && period.is_none() && day.is_none() {
        return Ok(None);
    }
    Ok(Some(ScheduleRule {
        start_date,
        period,
        day_num: day,
    }))
}

fn main_rate_spec(r: &DealRow) -> Result<RateSpec> {
    Ok(RateSpec {
        rate_type: RateType::parse(&r.rate_type_cd)?,
        interest_rate: r.interest_rate,
        indicator: non_empty(&r.base_indicator).map(String::from),
        indicator_rate: r.base_indicator_rate,
        payment_rule: schedule_rule(
            &r.interest_payment_rule_start_dttm,
            &r.interest_payment_rule_period_cd,
            r.interest_payment_rule_day_num,
            "interest_payment_rule",
        )?,
        repricing_rule: schedule_rule(
            &r.repricing_rule_start_dttm,
            &r.repricing_rule_period_cd,
            r.repricing_rule_day_num,
            "repricing_rule",
        )?,
        repricing_dates: opt(
            &r.repricing_dttm_schedule,
            "repricing_dttm_schedule",
            date_list,
        )?
        .unwrap_or_default(),
        rate_schedule: dated_rates(
            &r.repricing_dttm_rate_schedule,
            "repricing_dttm_rate_schedule",
        )?,
        fixed_rate_schedule: dated_rates(
            &r.repricing_dttm_rate_schedule_fixed,
            "repricing_dttm_rate_schedule_fixed",
        )?,
        cap: r.cap_limit_interest_rate,
        floor: r.floor_limit_interest_rate,
        coefficient: None,
        indicator_lag: opt(&r.indicator_lag_cd, "indicator_lag_cd", Lag::parse)?,
        first_repricing_event: None,
    })
}

fn sub_rate_spec(r: &DealRow) -> Result<Option<RateSpec>> {
    let Some(rate_type) = non_empty(&r.sub_rate_type_cd) else {
        return Ok(None);
    };
    let Some(interest_rate) = r.sub_interest_rate else {
        bail!("column \"sub_interest_rate\" is required")
    };
    Ok(Some(RateSpec {
        rate_type: RateType::parse(rate_type)?,
        interest_rate,
        indicator: non_empty(&r.sub_float_indicator).map(String::from),
        indicator_rate: None,
        payment_rule: schedule_rule(
            &r.sub_payment_rule_start_dttm,
            &r.sub_payment_rule_period_cd,
            r.sub_payment_rule_day_num,
            "sub_payment_rule",
        )?,
        repricing_rule: schedule_rule(
            &r.sub_repricing_rule_start_dttm,
            &r.sub_repricing_rule_period_cd,
            r.sub_repricing_rule_day_num,
            "sub_repricing_rule",
        )?,
        repricing_dates: vec![],
        rate_schedule: vec![],
        fixed_rate_schedule: vec![],
        cap: r.sub_cap_limit_interest_rate,
        floor: r.sub_floor_limit_interest_rate,
        coefficient: r.sub_coeff_index,
        indicator_lag: opt(&r.sub_indicator_lag_cd, "sub_indicator_lag_cd", Lag::parse)?,
        first_repricing_event: r.sub_repricing_rule_first_event_flg.map(|f| f != 0),
    }))
}

fn to_deal(r: &DealRow) -> Result<Deal> {
    let report_date = req_date(&r.report_dttm, "report_dttm")?;
    if let Some(drd) = opt(&r.deal_report_dttm, "deal_report_dttm", parse_date)? {
        if drd != report_date {
            bail!("report_dttm and deal_report_dttm differ: {report_date} != {drd}");
        }
    }
    let deal = Deal {
        deal_id: r.deal_id,
        deal_date: req_date(&r.deal_dttm, "deal_dttm")?,
        report_date,
        maturity_date: req_date(&r.maturity_dttm, "maturity_dttm")?,
        currency: r.currency_id.trim().to_string(),
        discriminator: Discriminator::parse(&r.discriminator)?,
        start_principal: r.start_principal_amt,
        outstanding_principal: r.current_principal_amt,
        accrued_interest: r.accrued_interest,
        day_count: DayCount::parse(&r.day_convention_cd)?,
        rate_spec: main_rate_spec(r)?,
        sub_rate_spec: sub_rate_spec(r)?,
        principal_rule: schedule_rule(
            &r.regular_payment_rule_start_dttm,
            &r.regular_payment_rule_period_cd,
            r.regular_payment_rule_day_num,
            "regular_payment_rule",
        )?,
        interest_payment_dates: opt(&r.interest_payment, "interest_payment", date_list)?
            .unwrap_or_default(),
        principal_payments: opt(&r.principal_payment, "principal_payment", dated_pairs)?
            .unwrap_or_default()
            .into_iter()
            .map(|(date, amount)| PrincipalPayment { date, amount })
            .collect(),
        product: non_empty(&r.product).map(String::from),
        behavior_model: non_empty(&r.behavior_model_cd).map(String::from),
    };
    validate_deal(&deal)?;
    Ok(deal)
}

fn validate_deal(deal: &Deal) -> Result<()> {
    if deal.deal_date > deal.report_date {
        bail!(
            "deal_date {} is after report_date {}",
            deal.deal_date,
            deal.report_date
        );
    }
    if deal.maturity_date <= deal.report_date {
        bail!(
            "maturity_date {} must be after report_date {}",
            deal.maturity_date,
            deal.report_date
        );
    }
    if deal.currency.is_empty() {
        bail!("currency_id is empty");
    }
    for (name, value) in [
        ("current_principal_amt", deal.outstanding_principal),
        ("interest_rate", deal.rate_spec.interest_rate),
    ] {
        if !value.is_finite() {
            bail!("{name} must be finite");
        }
    }
    Ok(())
}

pub fn parse_deals(path: &Path) -> Result<Vec<Deal>> {
    let mut deals = Vec::new();
    for_each_deal_chunk(path, usize::MAX, |mut chunk| {
        deals.append(&mut chunk);
        Ok(())
    })?;
    Ok(deals)
}

pub fn for_each_deal_chunk(
    path: &Path,
    chunk_size: usize,
    mut consume: impl FnMut(Vec<Deal>) -> Result<()>,
) -> Result<()> {
    if chunk_size == 0 {
        bail!("chunk size must be greater than zero");
    }
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .trim(csv::Trim::All)
        .from_path(path)
        .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;

    let mut deals = Vec::with_capacity(chunk_size.min(100_000));
    let mut seen = HashSet::new();
    for row in reader.deserialize::<DealRow>() {
        let row = row.map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
        if !seen.insert(row.deal_id) {
            bail!("duplicate deal_id value: {}", row.deal_id);
        }
        deals.push(to_deal(&row).map_err(|e| e.context(format!("deal_id={}", row.deal_id)))?);
        if deals.len() == chunk_size {
            consume(std::mem::take(&mut deals))?;
            deals = Vec::with_capacity(chunk_size.min(100_000));
        }
    }
    if !deals.is_empty() {
        consume(deals)?;
    }
    Ok(())
}
