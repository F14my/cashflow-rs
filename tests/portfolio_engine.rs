use cashflow::application::portfolio::{CurveSetTables, PortfolioStore, ValuationQuery};
use cashflow::application::valuation::ValuationService;
use cashflow::domain::cashflow::CalcConfig;
use cashflow::domain::curves::{Compounding, CurvePoint, CurveSet, ZeroCurve};
use cashflow::domain::indicators::IndicatorHistory;
use cashflow::infrastructure::csv_deals::{parse_deal_batch, parse_deals};
use cashflow::infrastructure::csv_indicators::load_indicator_csv;
use chrono::NaiveDate;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn curve(date: &str, scenario: &str, short: f64, long: f64) -> ZeroCurve {
    ZeroCurve::new_named(
        "API",
        d(date),
        "RUB",
        scenario,
        vec![
            CurvePoint { tenor_days: 90, zero_rate: short },
            CurvePoint { tenor_days: 3650, zero_rate: long },
        ],
        Compounding::Annual,
    )
    .unwrap()
}

fn indicators() -> Arc<IndicatorHistory> {
    let mut h = IndicatorHistory::new();
    load_indicator_csv(&mut h, "CBKEY1M", Path::new("data/cbkey1m.csv")).unwrap();
    Arc::new(h)
}

fn csv_rows() -> (Vec<String>, Vec<Vec<String>>) {
    let mut r = csv::ReaderBuilder::new().delimiter(b';').from_path("data/deals.csv").unwrap();
    let cols = r.headers().unwrap().iter().map(String::from).collect();
    let rows = r.records().map(|x| x.unwrap().iter().map(String::from).collect()).collect();
    (cols, rows)
}

#[test]
fn portfolio_totals_match_the_stateless_valuation() {
    let ind = indicators();
    let store = PortfolioStore::new(2, Duration::from_secs(60));
    let p = store.create(CalcConfig::default(), ind.clone(), 4);
    let (cols, rows) = csv_rows();
    // two batches, like a client streaming the file
    let (a, b) = rows.split_at(rows.len() / 2);
    for part in [a, b] {
        let batch = parse_deal_batch(&cols, part, Some("RUB"), 4).unwrap();
        p.upload(batch).unwrap();
    }
    let sealed = p.seal(&[d("2022-08-31")]).unwrap();
    assert!(sealed.valid > 40, "valid={}", sealed.valid);

    let scenarios = vec!["BASE".to_string(), "UP".to_string()];
    let sets = vec![CurveSetTables::new(
        "own".into(),
        vec![curve("2022-08-31", "BASE", 0.08, 0.09), curve("2022-08-31", "UP", 0.10, 0.11)],
        &scenarios,
        20_000,
    )
    .unwrap()];
    let q = ValuationQuery {
        as_of: d("2022-08-31"),
        sets,
        scenarios: scenarios.clone(),
        sample: HashSet::new(),
        only: None,
        keep: None,
    };
    let out = p.value(&q).unwrap();

    // reference: old stateless service over the same RUB deals, deals ok in every scenario
    let deals: Vec<_> = parse_deals(Path::new("data/deals.csv"))
        .unwrap()
        .into_iter()
        .filter(|x| x.currency == "RUB")
        .collect();
    let set = CurveSet::from_curves(vec![
        curve("2022-08-31", "BASE", 0.08, 0.09),
        curve("2022-08-31", "UP", 0.10, 0.11),
    ])
    .unwrap();
    let svc = ValuationService::new(ind.as_ref(), &set, CalcConfig::default(), 4);
    let vals = svc.value(&deals, "API", &scenarios);
    let ok: Vec<_> = vals.iter().filter(|v| v.scenarios.iter().all(|s| s.is_ok())).collect();
    assert_eq!(out.valued, ok.len());
    for (j, _) in scenarios.iter().enumerate() {
        let want: f64 = ok.iter().map(|v| v.scenarios[j].as_ref().unwrap().summary.npv).sum();
        let got = out.totals[0][j];
        assert!((want - got).abs() < 1e-6 * want.abs().max(1.0), "scenario {j}: {got} vs {want}");
    }
}

#[test]
fn rolling_drops_matured_deals_and_keeps_movers() {
    let p = PortfolioStore::new(1, Duration::from_secs(60)).create(CalcConfig::default(), indicators(), 2);
    let (cols, rows) = csv_rows();
    p.upload(parse_deal_batch(&cols, &rows, Some("RUB"), 2).unwrap()).unwrap();
    p.seal(&[d("2022-08-31"), d("2023-02-28")]).unwrap();
    let scenarios = vec!["BASE".to_string()];
    let value = |as_of: &str, keep: &str| {
        let sets = vec![CurveSetTables::new("own".into(), vec![curve(as_of, "BASE", 0.08, 0.09)], &scenarios, 20_000).unwrap()];
        p.value(&ValuationQuery {
            as_of: d(as_of),
            sets,
            scenarios: scenarios.clone(),
            sample: HashSet::new(),
            only: None,
            keep: Some(keep.into()),
        })
        .unwrap()
    };
    let start = value("2022-08-31", "a");
    let later = value("2023-02-28", "b");
    assert!(later.matured > 0, "some short deals must mature by 2023-02-28");
    assert_eq!(start.valued, later.valued + later.matured + (later.failed_total - start.failed_total));
    let movers = p.movers("a", "b", 3).unwrap();
    assert_eq!(movers.len(), 3);
    assert!(movers[0].change.abs() >= movers[2].change.abs());
}
