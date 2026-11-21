//! Load test of the portfolio engine: `cargo run --release --example portfolio_bench -- deals.csv`
use cashflow::application::portfolio::{CurveSetTables, PortfolioStore, ValuationQuery};
use cashflow::domain::cashflow::CalcConfig;
use cashflow::domain::curves::{Compounding, CurvePoint, ZeroCurve};
use cashflow::domain::indicators::IndicatorHistory;
use cashflow::infrastructure::csv_deals::parse_deal_batch;
use cashflow::infrastructure::csv_indicators::load_indicator_csv;
use chrono::{Months, NaiveDate};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SCEN: [&str; 7] = ["BASE", "PARALLEL_UP", "PARALLEL_DOWN", "STEEPENER", "FLATTENER", "SHORT_UP", "SHORT_DOWN"];

fn curve(date: NaiveDate, scenario: &str, shift: f64) -> ZeroCurve {
    let pts = [(30, 0.075), (365, 0.08), (1825, 0.09), (3650, 0.095), (10950, 0.1)]
        .iter()
        .map(|(t, r)| CurvePoint { tenor_days: *t, zero_rate: r + shift })
        .collect();
    ZeroCurve::new_named("API", date, "RUB", scenario, pts, Compounding::Annual).unwrap()
}

fn main() {
    let path = std::env::args().nth(1).expect("path to deals CSV");
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut h = IndicatorHistory::new();
    load_indicator_csv(&mut h, "CBKEY1M", Path::new("data/cbkey1m.csv")).unwrap();
    let p = PortfolioStore::new(1, Duration::from_secs(3600)).create(CalcConfig::default(), Arc::new(h), threads);

    let t = Instant::now();
    let mut r = csv::ReaderBuilder::new().delimiter(b';').from_path(&path).unwrap();
    let cols: Vec<String> = r.headers().unwrap().iter().map(String::from).collect();
    let mut rows = Vec::with_capacity(100_000);
    let flush = |rows: &mut Vec<Vec<String>>| {
        let b = parse_deal_batch(&cols, rows, Some("RUB"), threads).unwrap();
        p.upload(b).unwrap();
        rows.clear();
    };
    for rec in r.records() {
        rows.push(rec.unwrap().iter().map(String::from).collect());
        if rows.len() == 100_000 {
            flush(&mut rows);
        }
    }
    flush(&mut rows);
    println!("upload: {} deals in {:.1}s", p.deal_count(), t.elapsed().as_secs_f64());

    let start = NaiveDate::from_ymd_opt(2022, 8, 31).unwrap();
    let dates: Vec<NaiveDate> = (0..13).map(|m| start + Months::new(m)).collect();
    let t = Instant::now();
    let s = p.seal(&dates).unwrap();
    println!("seal: valid {} failed {} in {:.1}s", s.valid, s.failed_total, t.elapsed().as_secs_f64());

    let scen: Vec<String> = SCEN.iter().map(|s| s.to_string()).collect();
    for (as_of, n_sets) in [(dates[0], 1), (dates[6], 1), (dates[6], 16)] {
        let max = p.horizon_days(as_of).unwrap();
        let sets = (0..n_sets)
            .map(|k| {
                let curves = SCEN.iter().enumerate().map(|(j, s)| curve(as_of, s, 0.001 * (j + k) as f64)).collect();
                CurveSetTables::new(format!("s{k}"), curves, &scen, max).unwrap()
            })
            .collect();
        let t = Instant::now();
        let out = p
            .value(&ValuationQuery { as_of, sets, scenarios: scen.clone(), sample: HashSet::new(), only: None, keep: Some(as_of.to_string()) })
            .unwrap();
        println!(
            "value {as_of} sets={n_sets}: valued {} matured {} failed {} base={:.0} in {:.1}s",
            out.valued, out.matured, out.failed_total, out.totals[0][0], t.elapsed().as_secs_f64()
        );
    }
}
