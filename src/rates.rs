use crate::dates::{Date, DateExt, DayCount, Period};
use crate::error::Result;
use crate::indicators::IndicatorSource;
use crate::model::{Deal, RateSpec, RateType};

pub const DEFAULT_PROJECTED_INDICATOR: f64 = 0.08;

pub struct RateEngine<'a> {
    pub deal: &'a Deal,
    pub indicators: &'a dyn IndicatorSource,
    pub day_count: DayCount,
}

impl<'a> RateEngine<'a> {
    pub fn new(deal: &'a Deal, indicators: &'a dyn IndicatorSource, day_count: DayCount) -> Self {
        Self { deal, indicators, day_count }
    }

    pub fn year_fraction(&self, start: Date, end: Date) -> f64 {
        self.day_count.year_fraction(start, end)
    }

    pub fn repricing_dates_until(&self, spec: &RateSpec, end: Date) -> Result<Vec<Date>> {
        let report = self.deal.report_date;
        let mut out: Vec<Date> = spec.repricing_dates.iter().copied().filter(|d| report < *d && *d <= end).collect();
        if let Some(rule) = &spec.repricing_rule {
            if rule.period.is_some() {
                out.extend(rule.schedule(report, end, false)?);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn segment_boundaries(&self, spec: &RateSpec, start: Date, end: Date) -> Result<Vec<Date>> {
        let mut b: Vec<Date> = spec.repricing_dates.iter().copied().filter(|d| start < *d && *d < end).collect();
        if let Some(rule) = &spec.repricing_rule {
            if matches!(rule.period, Some(Period::Months(_)) | Some(Period::Days(_))) {
                b.extend(rule.dates_between(start, end)?.into_iter().filter(|d| *d < end));
            }
        }
        b.extend(
            spec.rate_schedule
                .iter()
                .chain(spec.fixed_rate_schedule.iter())
                .map(|r| r.date)
                .filter(|d| start < *d && *d < end),
        );
        b.sort();
        b.dedup();
        Ok(b)
    }

    fn apply_limits(spec: &RateSpec, mut rate: f64) -> f64 {
        if let Some(f) = spec.floor {
            rate = rate.max(f);
        }
        if let Some(c) = spec.cap {
            rate = rate.min(c);
        }
        rate
    }

    fn latest_repricing_date(&self, spec: &RateSpec, eff: Date) -> Result<Option<Date>> {
        let mut best: Option<Date> = spec.repricing_dates.iter().copied().filter(|d| *d <= eff).max();
        if let Some(rule) = &spec.repricing_rule {
            match (rule.period, rule.start_date) {
                (Some(Period::EndDate), _) => {
                    if self.deal.maturity_date <= eff {
                        best = best.max(Some(self.deal.maturity_date));
                    }
                }
                (Some(_), Some(start)) if start <= eff => {
                    let last = rule.dates_between(start.add_days(-1), eff)?.last().copied();
                    best = best.max(last);
                }
                _ => {}
            }
        }
        Ok(best)
    }

    fn scheduled_rate_on(&self, spec: &RateSpec, eff: Date) -> Result<Option<f64>> {
        let latest = spec
            .rate_schedule
            .iter()
            .chain(spec.fixed_rate_schedule.iter())
            .filter(|r| r.date <= eff)
            .max_by_key(|r| r.date);
        let Some(latest) = latest else { return Ok(None) };
        if spec.rate_type == RateType::Float && self.repricing_dates_until(spec, eff)?.iter().any(|d| *d > latest.date) {
            return Ok(None);
        }
        Ok(Some(latest.rate))
    }

    pub fn annual_rate_on(&self, spec: &RateSpec, eff: Date) -> Result<f64> {
        if let Some(r) = self.scheduled_rate_on(spec, eff)? {
            return Ok(Self::apply_limits(spec, r));
        }
        match spec.rate_type {
            RateType::Fixed => Ok(Self::apply_limits(spec, spec.interest_rate)),
            RateType::Float => {
                let indicator_value = if let Some(code) = &spec.indicator {
                    let repricing = self.latest_repricing_date(spec, eff)?;
                    let mut fixing = repricing.unwrap_or(eff);
                    if let Some(lag) = spec.indicator_lag {
                        fixing = lag.apply(fixing);
                    }
                    self.indicators.rate(code, fixing, self.deal.report_date)?
                } else if let Some(v) = spec.indicator_rate {
                    v
                } else {
                    DEFAULT_PROJECTED_INDICATOR
                };
                let coef = spec.coefficient.unwrap_or(1.0);
                Ok(Self::apply_limits(spec, spec.interest_rate + coef * indicator_value))
            }
        }
    }

    pub fn period_rate(&self, spec: &RateSpec, start: Date, end: Date) -> Result<f64> {
        if end <= start {
            return Ok(0.0);
        }
        let mut points = Vec::with_capacity(4);
        points.push(start);
        points.extend(self.segment_boundaries(spec, start, end)?);
        points.push(end);
        let mut acc = 0.0;
        for w in points.windows(2) {
            acc += self.annual_rate_on(spec, w[0])? * self.year_fraction(w[0], w[1]);
        }
        Ok(acc)
    }
}