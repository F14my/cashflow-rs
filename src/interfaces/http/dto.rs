use crate::application::valuation::{DealValuation, ScenarioTotal};
use crate::domain::valuation::DealSummary;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValuationRequest {
    /// Deal records with the same columns as `deals.csv`. Numbers or strings; null/"" are absent.
    pub deals: Vec<Map<String, Value>>,
    /// Discount curves, one per scenario (several dates per scenario are allowed).
    pub curves: Vec<CurveDto>,
    /// Scenarios to value; defaults to every scenario present in `curves`, in order.
    #[serde(default)]
    pub scenarios: Option<Vec<String>>,
    /// Extra or replacing indicator histories (default: the server's CBKEY1M history).
    #[serde(default)]
    pub indicators: Vec<IndicatorDto>,
    #[serde(default)]
    pub act_act_as_365: bool,
    #[serde(default)]
    pub include_cashflows: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveDto {
    pub scenario: String,
    pub curve_date: String,
    #[serde(default = "rub")]
    pub currency: String,
    /// `continuous` (default) or `annual`.
    #[serde(default)]
    pub compounding: Option<String>,
    pub points: Vec<CurvePointDto>,
}

fn rub() -> String {
    "RUB".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurvePointDto {
    pub tenor_days: u32,
    pub zero_rate_percent: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorDto {
    pub code: String,
    pub points: Vec<IndicatorPointDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndicatorPointDto {
    pub effective_date: String,
    pub rate_percent: f64,
}

#[derive(Debug, Serialize)]
pub struct SummaryDto {
    pub principal: f64,
    pub interest: f64,
    pub total_cf: f64,
    pub pv_principal: f64,
    pub pv_interest: f64,
    pub npv: f64,
}

impl From<&DealSummary> for SummaryDto {
    fn from(s: &DealSummary) -> Self {
        Self {
            principal: s.principal,
            interest: s.interest,
            total_cf: s.total_cf,
            pv_principal: s.pv_principal,
            pv_interest: s.pv_interest,
            npv: s.npv,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CashflowDto {
    pub date: String,
    pub principal: f64,
    pub interest: f64,
    pub total: f64,
    pub balance_before: f64,
    pub balance_after: f64,
    pub curve_date: String,
    pub discount_time: f64,
    pub zero_rate: f64,
    pub discount_factor: f64,
    pub pv_principal: f64,
    pub pv_interest: f64,
    pub pv: f64,
}

#[derive(Debug, Serialize)]
pub struct DealScenarioDto {
    pub scenario: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SummaryDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cashflows: Option<Vec<CashflowDto>>,
}

#[derive(Debug, Serialize)]
pub struct DealDto {
    pub deal_id: i64,
    pub scenarios: Vec<DealScenarioDto>,
}

#[derive(Debug, Serialize)]
pub struct ScenarioTotalDto {
    pub scenario: String,
    pub total: SummaryDto,
    pub ok: usize,
    pub failed: usize,
}

impl From<&ScenarioTotal> for ScenarioTotalDto {
    fn from(t: &ScenarioTotal) -> Self {
        Self {
            scenario: t.scenario.clone(),
            total: (&t.total).into(),
            ok: t.ok,
            failed: t.failed,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ValuationResponse {
    pub scenarios: Vec<ScenarioTotalDto>,
    pub deals: Vec<DealDto>,
    pub deal_count: usize,
    pub elapsed_ms: f64,
}

pub fn deal_dto(v: &DealValuation, scenarios: &[String], include_cashflows: bool) -> DealDto {
    DealDto {
        deal_id: v.deal_id,
        scenarios: v
            .scenarios
            .iter()
            .zip(scenarios)
            .map(|(r, name)| match r {
                Ok(d) => DealScenarioDto {
                    scenario: name.clone(),
                    summary: Some((&d.summary).into()),
                    error: None,
                    cashflows: include_cashflows.then(|| {
                        d.cashflows
                            .iter()
                            .map(|c| CashflowDto {
                                date: c.cashflow.cashflow_date.to_string(),
                                principal: c.cashflow.principal_cf,
                                interest: c.cashflow.interest_cf,
                                total: c.cashflow.total(),
                                balance_before: c.cashflow.balance_before,
                                balance_after: c.cashflow.balance_after,
                                curve_date: c.curve_date.to_string(),
                                discount_time: c.discount_time,
                                zero_rate: c.zero_rate,
                                discount_factor: c.discount_factor,
                                pv_principal: c.discounted_principal_cf,
                                pv_interest: c.discounted_interest_cf,
                                pv: c.discounted_total_cf,
                            })
                            .collect()
                    }),
                },
                Err(e) => DealScenarioDto {
                    scenario: name.clone(),
                    summary: None,
                    error: Some(e.to_string()),
                    cashflows: None,
                },
            })
            .collect(),
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorDto {
    pub code: &'static str,
    pub message: String,
}

/// Flattens JSON records into CSV-like columns and string cells.
pub fn records_to_table(records: &[Map<String, Value>]) -> (Vec<String>, Vec<Vec<String>>) {
    let mut columns: Vec<String> = Vec::new();
    for r in records {
        for k in r.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    let rows = records
        .iter()
        .map(|r| {
            columns
                .iter()
                .map(|c| match r.get(c) {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Bool(b)) => (if *b { "1" } else { "0" }).into(),
                    Some(v) => v.to_string(),
                })
                .collect()
        })
        .collect();
    (columns, rows)
}
