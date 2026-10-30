use crate::error::{bail, Error, Result};
use chrono::{Datelike, Days, Months, NaiveDate};

pub type Date = NaiveDate;

pub fn parse_date(s: &str) -> Result<Date> {
    let s = s.trim();
    let s = s.split([' ', 'T']).next().unwrap_or(s);
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(s, "%d.%m.%Y"))
        .map_err(|_| Error::new(format!("unsupported date format: {s:?}")))
}

pub trait DateExt {
    fn add_days(self, n: i32) -> Date;
    fn add_months(self, n: i32) -> Date;
    fn days_until(self, other: Date) -> i32;
}

impl DateExt for Date {
    fn add_days(self, n: i32) -> Date {
        if n >= 0 {
            self + Days::new(n as u64)
        } else {
            self - Days::new(n.unsigned_abs() as u64)
        }
    }

    fn add_months(self, n: i32) -> Date {
        if n >= 0 {
            self + Months::new(n as u32)
        } else {
            self - Months::new(n.unsigned_abs())
        }
    }

    fn days_until(self, other: Date) -> i32 {
        (other - self).num_days() as i32
    }
}

fn last_day_of_month(dt: Date) -> u32 {
    let first = dt.with_day(1).expect("day 1 exists");
    (first + Months::new(1) - Days::new(1)).day()
}

pub fn correct_date(day_num: i32, dt: Date) -> Result<Date> {
    let last = last_day_of_month(dt);
    let day = match day_num {
        -1 => last,
        1..=31 => (day_num as u32).min(last),
        _ => bail!("invalid day number: {day_num}"),
    };
    Ok(dt.with_day(day).expect("valid day"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Months(u32),
    Days(u32),
    EndDate,
}

impl Period {
    pub fn parse(s: &str) -> Result<Period> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("ENDDATE") {
            return Ok(Period::EndDate);
        }
        let (num, unit) = s.split_at(s.len().saturating_sub(1));
        let n: u32 = num
            .parse()
            .map_err(|_| Error::new(format!("unsupported period: {s:?}")))?;
        if n == 0 {
            bail!("unsupported period: {s:?}");
        }
        match unit {
            "M" | "m" => Ok(Period::Months(n)),
            "D" | "d" => Ok(Period::Days(n)),
            _ => bail!("unsupported period: {s:?}"),
        }
    }

    pub fn nominal_year_fraction(self) -> Option<f64> {
        match self {
            Period::Months(n) => Some(n as f64 / 12.0),
            Period::Days(n) => Some(n as f64 / 365.0),
            Period::EndDate => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleRule {
    pub start_date: Option<Date>,
    pub period: Option<Period>,
    pub day_num: Option<i32>,
}

impl ScheduleRule {
    pub fn dates_between(&self, after: Date, end: Date) -> Result<Vec<Date>> {
        let mut out = Vec::new();
        if end <= after {
            return Ok(out);
        }
        let period = match self.period {
            Some(p) => p,
            None => bail!("schedule period is required"),
        };
        if period == Period::EndDate {
            out.push(end);
            return Ok(out);
        }
        let anchor = match self.start_date {
            Some(d) => d,
            None => bail!("start date is required for period {period:?}"),
        };
        match period {
            Period::Months(step) => {
                let day = self.day_num.unwrap_or(anchor.day() as i32);
                let mut k: i32 = 0;
                let gap = (after.year() * 12 + after.month() as i32)
                    - (anchor.year() * 12 + anchor.month() as i32)
                    - 1;
                if gap > 0 {
                    k = (gap / step as i32) * step as i32;
                }
                loop {
                    let cand = correct_date(day, anchor.add_months(k))?;
                    if cand > end {
                        break;
                    }
                    if cand >= anchor && cand > after {
                        out.push(cand);
                    }
                    k += step as i32;
                }
            }
            Period::Days(step) => {
                if self.day_num.is_some() {
                    bail!("day_num is not supported for day periods");
                }
                let mut cand = anchor;
                if after > anchor {
                    let skip = anchor.days_until(after) / step as i32;
                    cand = anchor.add_days(skip * step as i32);
                }
                while cand <= end {
                    if cand > after {
                        out.push(cand);
                    }
                    cand = cand.add_days(step as i32);
                }
            }
            Period::EndDate => unreachable!(),
        }
        Ok(out)
    }

    pub fn schedule(&self, after: Date, end: Date, include_end: bool) -> Result<Vec<Date>> {
        let mut dates = self.dates_between(after, end)?;
        if include_end && end > after && dates.last() != Some(&end) {
            dates.push(end);
        }
        Ok(dates)
    }

    pub fn last_on_or_before(&self, limit: Date) -> Option<Date> {
        let anchor = self.start_date?;
        match self.period? {
            Period::EndDate => None,
            _ if anchor > limit => None,
            _ => {
                let before_anchor = anchor.add_days(-1);
                self.dates_between(before_anchor, limit)
                    .ok()?
                    .last()
                    .copied()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayCount {
    Act365,
    ActActIsda,
}

impl DayCount {
    pub fn parse(s: &str) -> Result<DayCount> {
        match s.trim().to_ascii_uppercase().as_str() {
            "ACT/365" | "ACT/365F" | "ACT/365 FIXED" => Ok(DayCount::Act365),
            "ACT/ACT" | "ACT/ACT ISDA" => Ok(DayCount::ActActIsda),
            other => bail!("unsupported day-count convention: {other:?}"),
        }
    }

    pub fn year_fraction(self, start: Date, end: Date) -> f64 {
        if end <= start {
            return 0.0;
        }
        match self {
            DayCount::Act365 => start.days_until(end) as f64 / 365.0,
            DayCount::ActActIsda => {
                let mut acc = 0.0;
                let mut cur = start;
                while cur < end {
                    let y = cur.year();
                    let next_year = NaiveDate::from_ymd_opt(y + 1, 1, 1).expect("valid date");
                    let seg_end = if end < next_year { end } else { next_year };
                    let denom = if NaiveDate::from_ymd_opt(y, 1, 1)
                        .expect("valid date")
                        .leap_year()
                    {
                        366.0
                    } else {
                        365.0
                    };
                    acc += cur.days_until(seg_end) as f64 / denom;
                    cur = seg_end;
                }
                acc
            }
        }
    }
}
