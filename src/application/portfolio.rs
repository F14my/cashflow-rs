//! Server-side portfolios for large runs.
//!
//! A client uploads deals once, in batches of records, then asks for many valuations of the same
//! portfolio: on different report dates (deals are rolled along their own schedules) and
//! under many curve sets at once (Shapley coalitions). Only totals travel back, plus
//! per-deal values for the deals the client explicitly asks for.
//!
//! Work is split into blocks of deals that threads pick up from a shared counter, so heavy
//! deals (30-year mortgages) do not stall a thread; block results are reduced in block
//! order, so totals are identical from run to run.

use crate::domain::cashflow::{build_cashflows, CalcConfig};
use crate::domain::curves::ZeroCurve;
use crate::domain::dates::{Date, DateExt};
use crate::domain::discounting::DiscountTable;
use crate::domain::error::{bail, Error, Result};
use crate::domain::indicators::IndicatorHistory;
use crate::domain::model::Deal;
use crate::domain::roll::{balance_on, roll_forward};
use crate::infrastructure::csv_deals::{DealBatch, RejectedRow};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Deals per work unit: small enough to balance load, large enough to keep overhead low.
const BLOCK: usize = 2048;
/// Errors kept verbatim per response; the rest are only counted.
const MAX_ERRORS: usize = 100;

/// Runs `f` over `0..n` split into blocks on `threads` threads; results come back in block order.
fn par_blocks<T: Send>(n: usize, threads: usize, f: impl Fn(Range<usize>) -> T + Sync) -> Vec<T> {
    let blocks = n.div_ceil(BLOCK);
    let threads = threads.clamp(1, blocks.max(1));
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, T)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let b = next.fetch_add(1, Ordering::Relaxed);
                        if b >= blocks {
                            break mine;
                        }
                        let range = b * BLOCK..((b + 1) * BLOCK).min(n);
                        mine.push((b, f(range)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("portfolio worker panicked"))
            .collect()
    });
    out.sort_by_key(|(b, _)| *b);
    out.into_iter().map(|(_, t)| t).collect()
}

#[derive(Clone, Debug)]
pub struct DealError {
    pub deal_id: i64,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct UploadReport {
    pub accepted: usize,
    pub skipped: usize,
    pub rejected_total: usize,
    pub rejected: Vec<RejectedRow>,
    pub total: usize,
}

#[derive(Debug)]
pub struct SealReport {
    pub deals: usize,
    pub valid: usize,
    pub failed_total: usize,
    pub failures: Vec<DealError>,
    pub report_dates: Vec<Date>,
    pub rejected_total: usize,
    pub rejected: Vec<RejectedRow>,
    pub elapsed: Duration,
}

/// Curves of one coalition / scenario set, by currency, one table per requested scenario.
pub struct CurveSetTables {
    pub name: String,
    by_currency: Vec<(String, Vec<DiscountTable>)>,
}

impl CurveSetTables {
    /// `curves` must contain one curve per (currency, scenario); tables are ordered like `scenarios`.
    pub fn new(name: String, curves: Vec<ZeroCurve>, scenarios: &[String], max_days: u32) -> Result<Self> {
        let mut by_ccy: HashMap<String, HashMap<String, ZeroCurve>> = HashMap::new();
        for c in curves {
            let slot = by_ccy.entry(c.currency.clone()).or_default();
            if slot.insert(c.scenario.clone(), c).is_some() {
                bail!("curve set {name:?}: duplicate curve for one scenario");
            }
        }
        let mut by_currency = Vec::new();
        for (ccy, mut curves) in by_ccy {
            let mut tables = Vec::with_capacity(scenarios.len());
            for s in scenarios {
                let curve = curves
                    .remove(s)
                    .ok_or_else(|| Error::new(format!("curve set {name:?}: no {ccy} curve for scenario {s}")))?;
                tables.push(DiscountTable::new(curve, max_days)?);
            }
            by_currency.push((ccy, tables));
        }
        Ok(Self { name, by_currency })
    }

    fn tables(&self, currency: &str) -> Option<&[DiscountTable]> {
        self.by_currency
            .iter()
            .find(|(c, _)| c.eq_ignore_ascii_case(currency))
            .map(|(_, t)| t.as_slice())
    }
}

pub struct ValuationQuery {
    pub as_of: Date,
    pub sets: Vec<CurveSetTables>,
    pub scenarios: Vec<String>,
    /// Per-deal values (first curve set only) for these deals.
    pub sample: HashSet<i64>,
    /// Value only these deals and return their values under every set.
    pub only: Option<HashSet<i64>>,
    /// Remember per-deal NPV of the first set's base scenario under this key (for movers).
    pub keep: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DealValues {
    pub deal_id: i64,
    pub outstanding: f64,
    /// `[set][scenario]` NPV; one set unless the query used `only`.
    pub npv: Vec<Vec<f64>>,
}

#[derive(Debug)]
pub struct ValuationOutcome {
    pub as_of: Date,
    pub valued: usize,
    pub matured: usize,
    pub failed_total: usize,
    pub failures: Vec<DealError>,
    /// `[set][scenario]` portfolio NPV over deals valued in every set and scenario.
    pub totals: Vec<Vec<f64>>,
    pub deals: Vec<DealValues>,
    pub elapsed: Duration,
}

/// How many deals prepay (by behaviour model) and how many carry a subsidy leg.
#[derive(Clone, Debug, Default)]
pub struct BehaviourStats {
    pub prepayment_enabled: bool,
    /// Deals with a behaviour model the policy prepays.
    pub prepaying: usize,
    /// `code -> (deals, annual CPR)`.
    pub by_model: std::collections::BTreeMap<String, (usize, f64)>,
    /// Deals whose model code the policy does not know (kept contractual).
    pub unknown_models: std::collections::BTreeMap<String, usize>,
    pub subsidised: usize,
    pub subsidised_principal: f64,
}

#[derive(Clone, Debug)]
pub struct Mover {
    pub deal_id: i64,
    pub before: f64,
    pub after: f64,
    pub change: f64,
}

struct Sealed {
    valid: Vec<bool>,
    failed_total: usize,
    failures: Vec<DealError>,
    /// Scheduled balance of every deal on a report date (NaN where the base schedule failed).
    balances: HashMap<Date, Arc<Vec<f64>>>,
    /// Per-deal NPV kept by valuations (NaN = not valued).
    kept: HashMap<String, Arc<Vec<f64>>>,
    max_days_from_report: u32,
}

#[derive(Default)]
struct Inner {
    deals: Vec<Deal>,
    ids: HashSet<i64>,
    report_dates: BTreeSet<Date>,
    rejected_total: usize,
    rejected: Vec<RejectedRow>,
    sealed: Option<Sealed>,
}

pub struct Portfolio {
    pub id: String,
    cfg: CalcConfig,
    indicators: Arc<IndicatorHistory>,
    threads: usize,
    inner: RwLock<Inner>,
    last_used: Mutex<Instant>,
}

impl Portfolio {
    fn touch(&self) {
        *self.last_used.lock().expect("clock lock") = Instant::now();
    }

    pub fn deal_count(&self) -> usize {
        self.inner.read().expect("portfolio lock").deals.len()
    }

    pub fn is_sealed(&self) -> bool {
        self.inner.read().expect("portfolio lock").sealed.is_some()
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub fn behaviour(&self) -> BehaviourStats {
        let inner = self.inner.read().expect("portfolio lock");
        let mut s = BehaviourStats { prepayment_enabled: self.cfg.prepayment.is_some(), ..Default::default() };
        for d in &inner.deals {
            if d.sub_rate_spec.is_some() {
                s.subsidised += 1;
                s.subsidised_principal += d.outstanding_principal;
            }
            let Some(code) = d.behavior_model.as_deref().map(|c| c.trim().to_ascii_uppercase()) else {
                continue;
            };
            match self.cfg.cpr(d) {
                Some(cpr) => {
                    s.prepaying += 1;
                    let e = s.by_model.entry(code).or_insert((0, cpr));
                    e.0 += 1;
                }
                None if s.prepayment_enabled => *s.unknown_models.entry(code).or_insert(0) += 1,
                None => {}
            }
        }
        s
    }

    /// Adds one parsed batch. Duplicate ids and unparsable rows are rejected individually.
    pub fn upload(&self, batch: DealBatch) -> Result<UploadReport> {
        self.touch();
        let mut inner = self.inner.write().expect("portfolio lock");
        if inner.sealed.is_some() {
            bail!("portfolio is sealed: deals can no longer be added");
        }
        let mut report = UploadReport { skipped: batch.skipped, ..Default::default() };
        let mut rejected = batch.rejected;
        inner.deals.reserve(batch.deals.len());
        for deal in batch.deals {
            if !inner.ids.insert(deal.deal_id) {
                rejected.push(RejectedRow {
                    line: 0,
                    deal_id: Some(deal.deal_id),
                    error: format!("duplicate deal_id value: {}", deal.deal_id),
                });
                continue;
            }
            inner.report_dates.insert(deal.report_date);
            inner.deals.push(deal);
            report.accepted += 1;
        }
        report.rejected_total = rejected.len();
        let room = MAX_ERRORS.saturating_sub(inner.rejected.len());
        inner.rejected.extend(rejected.iter().take(room).cloned());
        inner.rejected_total += rejected.len();
        rejected.truncate(MAX_ERRORS);
        report.rejected = rejected;
        report.total = inner.deals.len();
        Ok(report)
    }

    /// Builds every deal's base schedule once, marks deals whose schedule fails, and
    /// stores scheduled balances on `dates` for rolling. Idempotent for new dates.
    pub fn seal(&self, dates: &[Date]) -> Result<SealReport> {
        self.touch();
        let started = Instant::now();
        {
            let inner = self.inner.read().expect("portfolio lock");
            if inner.deals.is_empty() {
                bail!("portfolio has no deals");
            }
        }
        if self.is_sealed() {
            self.ensure_balances(dates)?;
        } else {
            let mut inner = self.inner.write().expect("portfolio lock");
            if inner.sealed.is_none() {
                let sealed = self.build_sealed(&inner.deals, dates);
                inner.sealed = Some(sealed);
            }
        }
        let inner = self.inner.read().expect("portfolio lock");
        let sealed = inner.sealed.as_ref().expect("sealed above");
        Ok(SealReport {
            deals: inner.deals.len(),
            valid: sealed.valid.iter().filter(|v| **v).count(),
            failed_total: sealed.failed_total,
            failures: sealed.failures.clone(),
            report_dates: inner.report_dates.iter().copied().collect(),
            rejected_total: inner.rejected_total,
            rejected: inner.rejected.clone(),
            elapsed: started.elapsed(),
        })
    }

    fn build_sealed(&self, deals: &[Deal], dates: &[Date]) -> Sealed {
        struct Part {
            valid: Vec<bool>,
            balances: Vec<f64>, // row-major: deal × date
            failures: Vec<DealError>,
            failed: usize,
            max_days: u32,
        }
        let parts = par_blocks(deals.len(), self.threads, |range| {
            let mut p = Part {
                valid: Vec::with_capacity(range.len()),
                balances: Vec::with_capacity(range.len() * dates.len()),
                failures: Vec::new(),
                failed: 0,
                max_days: 0,
            };
            for deal in &deals[range] {
                match build_cashflows(deal, self.indicators.as_ref(), &self.cfg) {
                    Ok(flows) => {
                        p.valid.push(true);
                        for d in dates {
                            p.balances.push(balance_on(&flows, deal.outstanding_principal, *d));
                        }
                        if let Some(last) = flows.last() {
                            let days = deal.report_date.days_until(last.cashflow_date).max(0) as u32;
                            p.max_days = p.max_days.max(days);
                        }
                    }
                    Err(e) => {
                        p.valid.push(false);
                        p.balances.extend(std::iter::repeat(f64::NAN).take(dates.len()));
                        p.failed += 1;
                        if p.failures.len() < MAX_ERRORS {
                            p.failures.push(DealError { deal_id: deal.deal_id, error: e.to_string() });
                        }
                    }
                }
            }
            p
        });
        let mut valid = Vec::with_capacity(deals.len());
        let mut per_date: Vec<Vec<f64>> = dates.iter().map(|_| Vec::with_capacity(deals.len())).collect();
        let (mut failures, mut failed_total, mut max_days) = (Vec::new(), 0, 0);
        for p in parts {
            for row in p.balances.chunks(dates.len().max(1)) {
                for (k, v) in row.iter().enumerate().take(dates.len()) {
                    per_date[k].push(*v);
                }
            }
            valid.extend(p.valid);
            failed_total += p.failed;
            max_days = max_days.max(p.max_days);
            failures.extend(p.failures);
        }
        failures.truncate(MAX_ERRORS);
        Sealed {
            valid,
            failed_total,
            failures,
            balances: dates.iter().copied().zip(per_date.into_iter().map(Arc::new)).collect(),
            kept: HashMap::new(),
            max_days_from_report: max_days,
        }
    }

    fn ensure_balances(&self, dates: &[Date]) -> Result<()> {
        let missing: Vec<Date> = {
            let inner = self.inner.read().expect("portfolio lock");
            let sealed = inner.sealed.as_ref().ok_or_else(|| Error::new("portfolio is not sealed"))?;
            dates.iter().copied().filter(|d| !sealed.balances.contains_key(d)).collect()
        };
        if missing.is_empty() {
            return Ok(());
        }
        let computed: Vec<Vec<f64>> = {
            let inner = self.inner.read().expect("portfolio lock");
            let deals = &inner.deals;
            let parts = par_blocks(deals.len(), self.threads, |range| {
                let mut rows = Vec::with_capacity(range.len() * missing.len());
                for deal in &deals[range] {
                    match build_cashflows(deal, self.indicators.as_ref(), &self.cfg) {
                        Ok(flows) => rows.extend(missing.iter().map(|d| balance_on(&flows, deal.outstanding_principal, *d))),
                        Err(_) => rows.extend(std::iter::repeat(f64::NAN).take(missing.len())),
                    }
                }
                rows
            });
            let mut per_date: Vec<Vec<f64>> = missing.iter().map(|_| Vec::with_capacity(deals.len())).collect();
            for rows in parts {
                for row in rows.chunks(missing.len()) {
                    for (k, v) in row.iter().enumerate() {
                        per_date[k].push(*v);
                    }
                }
            }
            per_date
        };
        let mut inner = self.inner.write().expect("portfolio lock");
        let sealed = inner.sealed.as_mut().expect("sealed");
        for (d, v) in missing.into_iter().zip(computed) {
            sealed.balances.insert(d, Arc::new(v));
        }
        Ok(())
    }

    /// Longest cash-flow horizon in days from `as_of`, for sizing discount tables.
    pub fn horizon_days(&self, as_of: Date) -> Result<u32> {
        let inner = self.inner.read().expect("portfolio lock");
        let sealed = inner.sealed.as_ref().ok_or_else(|| Error::new("portfolio is not sealed: call seal first"))?;
        let report = inner.report_dates.iter().next().copied().unwrap_or(as_of);
        let shift = report.days_until(as_of).max(0) as u32;
        Ok(sealed.max_days_from_report.saturating_sub(shift) + 31)
    }

    pub fn value(&self, q: &ValuationQuery) -> Result<ValuationOutcome> {
        self.touch();
        let started = Instant::now();
        if q.sets.is_empty() {
            bail!("curve_sets must not be empty");
        }
        self.ensure_balances(&[q.as_of])?;
        let inner = self.inner.read().expect("portfolio lock");
        let sealed = inner.sealed.as_ref().ok_or_else(|| Error::new("portfolio is not sealed: call seal first"))?;
        if let Some(first) = inner.report_dates.iter().next() {
            if q.as_of < *first {
                bail!("as_of {} is before the portfolio report date {first}", q.as_of);
            }
        }
        let balances = sealed.balances.get(&q.as_of).cloned().expect("ensured above");
        let deals = &inner.deals;
        let (n_sets, n_scen) = (q.sets.len(), q.scenarios.len());
        let base_idx = q.scenarios.iter().position(|s| s == "BASE").unwrap_or(0);
        let per_deal_all_sets = q.only.is_some();

        struct Part {
            totals: Vec<f64>,
            valued: usize,
            matured: usize,
            failed: usize,
            failures: Vec<DealError>,
            deals: Vec<DealValues>,
            kept: Vec<f64>,
        }

        let parts = par_blocks(deals.len(), self.threads, |range| {
            let mut p = Part {
                totals: vec![0.0; n_sets * n_scen],
                valued: 0,
                matured: 0,
                failed: 0,
                failures: Vec::new(),
                deals: Vec::new(),
                kept: if q.keep.is_some() { vec![f64::NAN; range.len()] } else { Vec::new() },
            };
            let mut flows: Vec<(u32, f64)> = Vec::new();
            let mut npv = vec![0.0; n_sets * n_scen];
            for (k, i) in range.clone().enumerate() {
                let deal = &deals[i];
                if !sealed.valid[i] {
                    continue;
                }
                if let Some(only) = &q.only {
                    if !only.contains(&deal.deal_id) {
                        continue;
                    }
                }
                let rolled;
                let state = if q.as_of <= deal.report_date {
                    if deal.maturity_date <= q.as_of {
                        p.matured += 1;
                        continue;
                    }
                    deal
                } else {
                    match roll_forward(deal, balances[i], q.as_of, self.cfg.cpr(deal).is_some()) {
                        Some(r) => {
                            rolled = r;
                            &rolled
                        }
                        None => {
                            p.matured += 1;
                            continue;
                        }
                    }
                };
                let result = value_one(state, q, &self.indicators, &self.cfg, &mut flows, &mut npv);
                match result {
                    Ok(()) => {
                        p.valued += 1;
                        for (t, v) in p.totals.iter_mut().zip(&npv) {
                            *t += v;
                        }
                        if q.keep.is_some() {
                            p.kept[k] = npv[base_idx];
                        }
                        if per_deal_all_sets || q.sample.contains(&deal.deal_id) {
                            let sets = if per_deal_all_sets { n_sets } else { 1 };
                            p.deals.push(DealValues {
                                deal_id: deal.deal_id,
                                outstanding: state.outstanding_principal,
                                npv: (0..sets).map(|s| npv[s * n_scen..(s + 1) * n_scen].to_vec()).collect(),
                            });
                        }
                    }
                    Err(e) => {
                        p.failed += 1;
                        if p.failures.len() < MAX_ERRORS {
                            p.failures.push(DealError { deal_id: deal.deal_id, error: e.to_string() });
                        }
                    }
                }
            }
            p
        });

        let mut out = ValuationOutcome {
            as_of: q.as_of,
            valued: 0,
            matured: 0,
            failed_total: 0,
            failures: Vec::new(),
            totals: vec![vec![0.0; n_scen]; n_sets],
            deals: Vec::new(),
            elapsed: Duration::ZERO,
        };
        let mut kept = if q.keep.is_some() { Vec::with_capacity(deals.len()) } else { Vec::new() };
        for p in parts {
            for (idx, v) in p.totals.iter().enumerate() {
                out.totals[idx / n_scen][idx % n_scen] += v;
            }
            out.valued += p.valued;
            out.matured += p.matured;
            out.failed_total += p.failed;
            out.failures.extend(p.failures);
            out.deals.extend(p.deals);
            kept.extend(p.kept);
        }
        out.failures.truncate(MAX_ERRORS);
        drop(inner);
        if let Some(key) = &q.keep {
            let mut inner = self.inner.write().expect("portfolio lock");
            if let Some(sealed) = inner.sealed.as_mut() {
                sealed.kept.insert(key.clone(), Arc::new(kept));
            }
        }
        out.elapsed = started.elapsed();
        Ok(out)
    }

    /// Deals whose kept NPV changed the most between two valuations.
    pub fn movers(&self, from: &str, to: &str, n: usize) -> Result<Vec<Mover>> {
        let inner = self.inner.read().expect("portfolio lock");
        let sealed = inner.sealed.as_ref().ok_or_else(|| Error::new("portfolio is not sealed"))?;
        let a = sealed.kept.get(from).ok_or_else(|| Error::new(format!("no kept valuation {from:?}")))?;
        let b = sealed.kept.get(to).ok_or_else(|| Error::new(format!("no kept valuation {to:?}")))?;
        let mut rows: Vec<Mover> = inner
            .deals
            .iter()
            .enumerate()
            .filter(|(i, _)| !(a[*i].is_nan() && b[*i].is_nan()))
            .map(|(i, d)| {
                let before = if a[i].is_nan() { 0.0 } else { a[i] };
                let after = if b[i].is_nan() { 0.0 } else { b[i] };
                Mover { deal_id: d.deal_id, before, after, change: after - before }
            })
            .collect();
        let n = n.min(rows.len());
        if n == 0 {
            return Ok(Vec::new());
        }
        let key = |m: &Mover| -m.change.abs();
        rows.select_nth_unstable_by(n - 1, |x, y| key(x).total_cmp(&key(y)));
        rows.truncate(n);
        rows.sort_by(|x, y| key(x).total_cmp(&key(y)).then(x.deal_id.cmp(&y.deal_id)));
        Ok(rows)
    }
}

/// Schedule of one (rolled) deal, discounted by every set and scenario into `npv`.
fn value_one(
    deal: &Deal,
    q: &ValuationQuery,
    indicators: &IndicatorHistory,
    cfg: &CalcConfig,
    flows: &mut Vec<(u32, f64)>,
    npv: &mut [f64],
) -> Result<()> {
    let schedule = build_cashflows(deal, indicators, cfg).map_err(|e| e.context(format!("deal_id={}", deal.deal_id)))?;
    flows.clear();
    for cf in &schedule {
        let days = q.as_of.days_until(cf.cashflow_date);
        if days < 0 {
            bail!("deal_id={}: cash flow date {} is before report date {}", deal.deal_id, cf.cashflow_date, q.as_of);
        }
        flows.push((days as u32, cf.total()));
    }
    let n_scen = q.scenarios.len();
    for (s, set) in q.sets.iter().enumerate() {
        let tables = set
            .tables(&deal.currency)
            .ok_or_else(|| Error::new(format!("deal_id={}: curve set {:?} has no {} curves", deal.deal_id, set.name, deal.currency)))?;
        for (j, table) in tables.iter().enumerate() {
            let mut v = 0.0;
            for (days, amount) in flows.iter() {
                v += amount * table.factor(*days)?;
            }
            npv[s * n_scen + j] = v;
        }
    }
    Ok(())
}

pub struct PortfolioStore {
    map: Mutex<HashMap<String, Arc<Portfolio>>>,
    max: usize,
    idle: Duration,
    counter: AtomicU64,
}

impl PortfolioStore {
    /// Keeps at most `max` portfolios; idle ones are dropped first when room is needed.
    pub fn new(max: usize, idle: Duration) -> Self {
        Self { map: Mutex::new(HashMap::new()), max: max.max(1), idle, counter: AtomicU64::new(0) }
    }

    pub fn create(&self, cfg: CalcConfig, indicators: Arc<IndicatorHistory>, threads: usize) -> Arc<Portfolio> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let id = format!("p{:x}{:04x}", nanos, self.counter.fetch_add(1, Ordering::Relaxed) & 0xffff);
        let portfolio = Arc::new(Portfolio {
            id: id.clone(),
            cfg,
            indicators,
            threads: threads.max(1),
            inner: RwLock::new(Inner::default()),
            last_used: Mutex::new(Instant::now()),
        });
        let mut map = self.map.lock().expect("store lock");
        let idle = self.idle;
        map.retain(|_, p| p.last_used.lock().map(|t| t.elapsed() < idle).unwrap_or(false));
        while map.len() >= self.max {
            let oldest = map
                .iter()
                .min_by_key(|(_, p)| *p.last_used.lock().expect("clock lock"))
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    map.remove(&k);
                }
                None => break,
            }
        }
        map.insert(id, portfolio.clone());
        portfolio
    }

    pub fn get(&self, id: &str) -> Option<Arc<Portfolio>> {
        self.map.lock().expect("store lock").get(id).cloned()
    }

    pub fn remove(&self, id: &str) -> bool {
        self.map.lock().expect("store lock").remove(id).is_some()
    }
}
