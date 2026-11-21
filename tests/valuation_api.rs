use cashflow::application::valuation::{scenario_totals, ValuationService};
use cashflow::domain::cashflow::CalcConfig;
use cashflow::domain::curves::{Compounding, CurvePoint, CurveSet, ZeroCurve};
use cashflow::domain::indicators::IndicatorHistory;
use cashflow::infrastructure::csv_deals::{parse_deal_records, parse_deals};
use cashflow::infrastructure::csv_indicators::load_indicator_csv;
use cashflow::interfaces::http::dto::records_to_table;
use chrono::NaiveDate;
use serde_json::{json, Map, Value};
use std::path::Path;

fn flat(currency: &str, scenario: &str, rate: f64) -> ZeroCurve {
    ZeroCurve::new_named(
        "API",
        NaiveDate::from_ymd_opt(2022, 8, 31).unwrap(),
        currency,
        scenario,
        vec![CurvePoint {
            tenor_days: 365,
            zero_rate: rate,
        }],
        Compounding::Continuous,
    )
    .unwrap()
}

fn indicators() -> IndicatorHistory {
    let mut h = IndicatorHistory::new();
    load_indicator_csv(&mut h, "CBKEY1M", Path::new("data/cbkey1m.csv")).unwrap();
    h
}

#[test]
fn higher_rates_lower_npv_and_schedules_are_shared() {
    let deals = parse_deals(Path::new("data/deals.csv")).unwrap();
    let curves = CurveSet::from_curves(vec![
        flat("RUB", "BASE", 0.08),
        flat("RUB", "UP", 0.10),
        flat("USD", "BASE", 0.03),
        flat("USD", "UP", 0.05),
    ])
    .unwrap();
    let ind = indicators();
    let scenarios = vec!["BASE".to_string(), "UP".to_string()];
    let service = ValuationService::new(&ind, &curves, CalcConfig::default(), 4);
    let values = service.value(&deals, "API", &scenarios);
    assert_eq!(values.len(), deals.len());
    let totals = scenario_totals(&values, &scenarios);
    assert_eq!(totals[0].failed, 0);
    assert!(totals[1].total.npv < totals[0].total.npv);
    assert!((totals[1].total.total_cf - totals[0].total.total_cf).abs() < 1e-6);
}

#[test]
fn json_records_parse_like_csv_rows() {
    let from_csv = parse_deals(Path::new("data/deals.csv")).unwrap();
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_path("data/deals.csv")
        .unwrap();
    let headers: Vec<String> = reader.headers().unwrap().iter().map(String::from).collect();
    let records: Vec<Map<String, Value>> = reader
        .records()
        .map(|r| {
            let r = r.unwrap();
            headers
                .iter()
                .zip(r.iter())
                .map(|(h, v)| {
                    let value = if h == "deal_id" {
                        json!(v.parse::<i64>().unwrap())
                    } else {
                        json!(v)
                    };
                    (h.clone(), value)
                })
                .collect()
        })
        .collect();
    let (columns, rows) = records_to_table(&records);
    let from_json = parse_deal_records(&columns, &rows).unwrap();
    assert_eq!(from_csv, from_json);
}
