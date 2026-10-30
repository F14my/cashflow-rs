use crate::dates::{Date, DateExt, DayCount, Period};
use crate::error::{bail, Result};
use crate::indicators::IndicatorSource;
use crate::model::{Deal, RateSpec, RateType};

pub struct RateEngine<'a> {
    pub deal: &'a Deal,
    pub indicators: &'a dyn IndicatorSource,
    pub day_count: DayCount,
}

impl<'a> RateEngine<'a> {
    pub fn new(deal: &'a Deal, indicators: &'a dyn IndicatorSource, day_count: DayCount) -> Self {
        Self {
            deal,
            indicators,
            day_count,
        }
    }

    pub fn year_fraction(&self, start: Date, end: Date) -> f64 {
        self.day_count.year_fraction(start, end)
    }

    fn rule_repricing_dates(&self, spec: &RateSpec, after: Date, end: Date) -> Result<Vec<Date>> {
        let Some(rule) = &spec.repricing_rule else {
            return Ok(Vec::new());
        };
        if rule.period.is_none() {
            return Ok(Vec::new());
        }
        let mut dates = rule.dates_between(after, end)?;
        if spec.first_repricing_event == Some(false) {
            if let Some(first) = rule.start_date {
                dates.retain(|date| *date != first);
            }
        }
        Ok(dates)
    }

    pub fn repricing_dates_until(&self, spec: &RateSpec, end: Date) -> Result<Vec<Date>> {
        let report = self.deal.report_date;
        let mut out: Vec<Date> = spec
            .repricing_dates
            .iter()
            .copied()
            .filter(|d| report < *d && *d <= end)
            .collect();
        if let Some(rule) = &spec.repricing_rule {
            if rule.period.is_some() {
                out.extend(self.rule_repricing_dates(spec, report, end)?);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn segment_boundaries(&self, spec: &RateSpec, start: Date, end: Date) -> Result<Vec<Date>> {
        let mut b: Vec<Date> = spec
            .repricing_dates
            .iter()
            .copied()
            .filter(|d| start < *d && *d < end)
            .collect();
        if let Some(rule) = &spec.repricing_rule {
            if matches!(rule.period, Some(Period::Months(_)) | Some(Period::Days(_))) {
                b.extend(
                    self.rule_repricing_dates(spec, start, end)?
                        .into_iter()
                        .filter(|d| *d < end),
                );
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
        let mut best: Option<Date> = spec
            .repricing_dates
            .iter()
            .copied()
            .filter(|d| *d <= eff)
            .max();
        if let Some(rule) = &spec.repricing_rule {
            match (rule.period, rule.start_date) {
                (Some(Period::EndDate), _) => {
                    if self.deal.maturity_date <= eff {
                        best = best.max(Some(self.deal.maturity_date));
                    }
                }
                (Some(_), Some(start)) if start <= eff => {
                    let last = self
                        .rule_repricing_dates(spec, start.add_days(-1), eff)?
                        .last()
                        .copied();
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
        let Some(latest) = latest else {
            return Ok(None);
        };
        if spec.rate_type == RateType::Float
            && self
                .latest_repricing_date(spec, eff)?
                .is_some_and(|date| date > latest.date)
        {
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
                    bail!("FLOAT rate has neither an indicator nor base_indicator_rate")
                };
                let coef = spec.coefficient.unwrap_or(1.0);
                Ok(Self::apply_limits(
                    spec,
                    spec.interest_rate + coef * indicator_value,
                ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::ScheduleRule;
    use crate::error::Result;
    use crate::model::{DatedRate, Discriminator};
    use chrono::NaiveDate;

    struct ConstantIndicator(f64);

    impl IndicatorSource for ConstantIndicator {
        fn rate(&self, _indicator: &str, _fixing_date: Date, _report_date: Date) -> Result<f64> {
            Ok(self.0)
        }
    }

    fn date(year: i32, month: u32, day: u32) -> Date {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn float_spec() -> RateSpec {
        RateSpec {
            rate_type: RateType::Float,
            interest_rate: 0.01,
            indicator: Some("CBKEY1M".into()),
            indicator_rate: None,
            payment_rule: None,
            repricing_rule: None,
            repricing_dates: Vec::new(),
            rate_schedule: Vec::new(),
            fixed_rate_schedule: Vec::new(),
            cap: None,
            floor: None,
            coefficient: None,
            indicator_lag: None,
            first_repricing_event: None,
        }
    }

    fn deal(spec: RateSpec) -> Deal {
        Deal {
            deal_id: 1,
            deal_date: date(2020, 1, 1),
            report_date: date(2022, 8, 31),
            maturity_date: date(2025, 1, 1),
            currency: "RUB".into(),
            discriminator: Discriminator::Simple,
            start_principal: None,
            outstanding_principal: 100.0,
            accrued_interest: None,
            day_count: DayCount::Act365,
            rate_spec: spec,
            sub_rate_spec: None,
            principal_rule: None,
            interest_payment_dates: Vec::new(),
            principal_payments: Vec::new(),
            product: None,
            behavior_model: None,
        }
    }

    #[test]
    fn repricing_before_report_invalidates_older_scheduled_rate() {
        let mut spec = float_spec();
        spec.rate_schedule.push(DatedRate {
            date: date(2022, 1, 1),
            rate: 0.05,
        });
        spec.repricing_dates.push(date(2022, 6, 1));
        let deal = deal(spec);
        let indicator = ConstantIndicator(0.07);
        let engine = RateEngine::new(&deal, &indicator, DayCount::Act365);
        assert!(
            (engine
                .annual_rate_on(&deal.rate_spec, deal.report_date)
                .unwrap()
                - 0.08)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn float_without_indicator_is_an_error() {
        let mut spec = float_spec();
        spec.indicator = None;
        let deal = deal(spec);
        let indicator = ConstantIndicator(0.07);
        let engine = RateEngine::new(&deal, &indicator, DayCount::Act365);
        assert!(engine
            .annual_rate_on(&deal.rate_spec, deal.report_date)
            .is_err());
    }

    #[test]
    fn disabled_first_repricing_event_skips_rule_start() {
        let mut spec = float_spec();
        spec.repricing_rule = Some(ScheduleRule {
            start_date: Some(date(2022, 9, 1)),
            period: Some(Period::Months(1)),
            day_num: Some(1),
        });
        spec.first_repricing_event = Some(false);
        let deal = deal(spec);
        let indicator = ConstantIndicator(0.07);
        let engine = RateEngine::new(&deal, &indicator, DayCount::Act365);
        assert_eq!(
            engine
                .rule_repricing_dates(&deal.rate_spec, deal.report_date, date(2022, 10, 1))
                .unwrap(),
            vec![date(2022, 10, 1)]
        );
    }
}
