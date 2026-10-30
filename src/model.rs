use crate::dates::{Date, DateExt, DayCount, ScheduleRule};
use crate::error::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateType {
    Fixed,
    Float,
}

impl RateType {
    pub fn parse(s: &str) -> Result<RateType> {
        match s.trim().to_ascii_uppercase().as_str() {
            "FIXED" => Ok(RateType::Fixed),
            "FLOAT" => Ok(RateType::Float),
            other => bail!("unsupported rate type: {other:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discriminator {
    Simple,
    Rgm,
    Ann,
}

impl Discriminator {
    pub fn parse(s: &str) -> Result<Discriminator> {
        match s.trim().to_ascii_uppercase().as_str() {
            "SIMPLE" => Ok(Discriminator::Simple),
            "RGM" => Ok(Discriminator::Rgm),
            "ANN" => Ok(Discriminator::Ann),
            other => bail!("unsupported discriminator: {other:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lag {
    Months(i32),
    Days(i32),
}

impl Lag {
    pub fn parse(s: &str) -> Result<Lag> {
        let s = s.trim();
        let (num, unit) = s.split_at(s.len().saturating_sub(1));
        let n: i32 = num.trim_start_matches('+').parse().map_err(|_| crate::error::Error::new(format!("unsupported indicator lag: {s:?}")))?;
        match unit {
            "M" | "m" => Ok(Lag::Months(n)),
            "D" | "d" => Ok(Lag::Days(n)),
            _ => bail!("unsupported indicator lag: {s:?}"),
        }
    }

    pub fn apply(self, d: Date) -> Date {
        match self {
            Lag::Months(n) => d.add_months(n),
            Lag::Days(n) => d.add_days(n),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DatedRate {
    pub date: Date,
    pub rate: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrincipalPayment {
    pub date: Date,
    pub amount: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RateSpec {
    pub rate_type: RateType,
    pub interest_rate: f64,
    pub indicator: Option<String>,
    pub indicator_rate: Option<f64>,
    pub payment_rule: Option<ScheduleRule>,
    pub repricing_rule: Option<ScheduleRule>,
    pub repricing_dates: Vec<Date>,
    pub rate_schedule: Vec<DatedRate>,
    pub fixed_rate_schedule: Vec<DatedRate>,
    pub cap: Option<f64>,
    pub floor: Option<f64>,
    pub coefficient: Option<f64>,
    pub indicator_lag: Option<Lag>,
    pub first_repricing_event: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deal {
    pub deal_id: i64,
    pub deal_date: Date,
    pub report_date: Date,
    pub maturity_date: Date,
    pub currency: String,
    pub discriminator: Discriminator,
    pub start_principal: Option<f64>,
    pub outstanding_principal: f64,
    pub accrued_interest: Option<f64>,
    pub day_count: DayCount,
    pub rate_spec: RateSpec,
    pub sub_rate_spec: Option<RateSpec>,
    pub principal_rule: Option<ScheduleRule>,
    pub interest_payment_dates: Vec<Date>,
    pub principal_payments: Vec<PrincipalPayment>,
    pub product: Option<String>,
    pub behavior_model: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CashFlow {
    pub deal_id: i64,
    pub cashflow_date: Date,
    pub currency: String,
    pub interest_period_start: Date,
    pub interest_period_end: Date,
    pub principal_cf: f64,
    pub interest_cf: f64,
    pub balance_before: f64,
    pub balance_after: f64,
}

impl CashFlow {
    pub fn total(&self) -> f64 {
        self.principal_cf + self.interest_cf
    }
}