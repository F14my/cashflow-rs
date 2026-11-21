//! Portfolio endpoints: upload deals in JSON batches once, then value them many times.
//!
//! ```text
//! POST   /api/v1/portfolios                     {indicators?, act_act_as_365?}      -> 201 {id}
//! POST   /api/v1/portfolios/{id}/deals          {deals: [...], currency?}           -> upload report
//! POST   /api/v1/portfolios/{id}/seal           {dates: [...]}                      -> base schedules, balances
//! POST   /api/v1/portfolios/{id}/valuations     {as_of, curve_sets, scenarios?, sample_ids?, only_ids?, keep?}
//! GET    /api/v1/portfolios/{id}/movers?from=&to=&n=
//! GET    /api/v1/portfolios/{id}
//! DELETE /api/v1/portfolios/{id}
//! ```

use super::dto::{records_to_table, CurvePointDto, IndicatorDto};
use super::router::{invalid, merged_indicators, ApiError, AppState};
use crate::application::portfolio::{CurveSetTables, DealError, Portfolio, ValuationQuery};
use crate::domain::cashflow::CalcConfig;
use crate::domain::curves::{Compounding, CurvePoint, ZeroCurve};
use crate::domain::dates::{parse_date, Date};
use crate::domain::error::{Error, Result};
use crate::infrastructure::csv_deals::{parse_deal_batch, RejectedRow};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

const CURVE_TYPE: &str = "API";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePortfolioRequest {
    #[serde(default)]
    pub indicators: Vec<IndicatorDto>,
    #[serde(default)]
    pub act_act_as_365: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    /// Deal records with the columns of `deals.csv`; absent and empty fields are the same.
    pub deals: Vec<Map<String, Value>>,
    /// Keep only deals of this currency; the others are counted as skipped.
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealRequest {
    /// Report dates the portfolio will be rolled to (more can be added later).
    #[serde(default)]
    pub dates: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetCurveDto {
    pub scenario: String,
    #[serde(default = "rub")]
    pub currency: String,
    #[serde(default)]
    pub compounding: Option<String>,
    pub points: Vec<CurvePointDto>,
}

fn rub() -> String {
    "RUB".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveSetDto {
    pub name: String,
    pub curves: Vec<SetCurveDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortfolioValuationRequest {
    /// Valuation date; deals are rolled to it along their schedules.
    pub as_of: String,
    /// One or more curve sets (e.g. Shapley coalitions); each needs a curve per scenario.
    pub curve_sets: Vec<CurveSetDto>,
    #[serde(default)]
    pub scenarios: Option<Vec<String>>,
    /// Return per-deal values (first curve set) for these deals.
    #[serde(default)]
    pub sample_ids: Vec<i64>,
    /// Value only these deals; per-deal values come back for every set.
    #[serde(default)]
    pub only_ids: Option<Vec<i64>>,
    /// Keep per-deal base NPV under this key for `movers`.
    #[serde(default)]
    pub keep: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MoversQuery {
    pub from: String,
    pub to: String,
    #[serde(default = "ten")]
    pub n: usize,
}

fn ten() -> usize {
    10
}

#[derive(Debug, Serialize)]
pub struct RejectedDto {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deal_id: Option<i64>,
    pub error: String,
}

impl From<&RejectedRow> for RejectedDto {
    fn from(r: &RejectedRow) -> Self {
        Self { index: r.line, deal_id: r.deal_id, error: r.error.clone() }
    }
}

#[derive(Debug, Serialize)]
pub struct DealErrorDto {
    pub deal_id: i64,
    pub error: String,
}

impl From<&DealError> for DealErrorDto {
    fn from(e: &DealError) -> Self {
        Self { deal_id: e.deal_id, error: e.error.clone() }
    }
}

#[derive(Debug, Serialize)]
pub struct SetTotalsDto {
    pub name: String,
    pub npv: BTreeMap<String, f64>,
}

#[derive(Debug, Serialize)]
pub struct DealValuesDto {
    pub deal_id: i64,
    pub outstanding: f64,
    /// One map per curve set (only the first set unless `only_ids` was used).
    pub npv: Vec<BTreeMap<String, f64>>,
}

fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::result::Result<T, ApiError> + Send + 'static,
) -> impl std::future::Future<Output = std::result::Result<T, ApiError>> {
    async move {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e.to_string()))?
    }
}

fn find(state: &AppState, id: &str) -> std::result::Result<Arc<Portfolio>, ApiError> {
    state
        .portfolios
        .get(id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not_found", format!("portfolio {id} not found")))
}

pub async fn create(
    State(state): State<AppState>,
    body: Option<Json<CreatePortfolioRequest>>,
) -> std::result::Result<impl IntoResponse, ApiError> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let indicators = merged_indicators(&state, &req.indicators)?;
    let cfg = CalcConfig { act_act_as_365: req.act_act_as_365 };
    let p = state.portfolios.create(cfg, indicators, state.threads);
    Ok((StatusCode::CREATED, Json(serde_json::json!({"id": p.id}))))
}

pub async fn info(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = find(&state, &id)?;
    Ok(Json(serde_json::json!({"id": p.id, "deals": p.deal_count(), "sealed": p.is_sealed()})))
}

pub async fn remove(State(state): State<AppState>, Path(id): Path<String>) -> StatusCode {
    if state.portfolios.remove(&id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

pub async fn upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UploadRequest>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = find(&state, &id)?;
    blocking(move || {
        let (columns, rows) = records_to_table(&req.deals);
        let currency = req.currency.as_deref().map(str::trim).filter(|c| !c.is_empty());
        let batch = parse_deal_batch(&columns, &rows, currency, p.threads()).map_err(invalid)?;
        let r = p.upload(batch).map_err(conflict)?;
        Ok(Json(serde_json::json!({
            "accepted": r.accepted,
            "skipped": r.skipped,
            "rejected_total": r.rejected_total,
            "rejected": r.rejected.iter().map(RejectedDto::from).collect::<Vec<_>>(),
            "total": r.total,
        })))
    })
    .await
}

fn conflict(e: Error) -> ApiError {
    ApiError(StatusCode::CONFLICT, "conflict", e.to_string())
}

fn dates(list: &[String]) -> Result<Vec<Date>> {
    list.iter().map(|d| parse_date(d)).collect()
}

pub async fn seal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<SealRequest>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = find(&state, &id)?;
    let req_dates = body.map(|Json(b)| b.dates).unwrap_or_default();
    blocking(move || {
        let dates = dates(&req_dates).map_err(invalid)?;
        let r = p.seal(&dates).map_err(conflict)?;
        Ok(Json(serde_json::json!({
            "deals": r.deals,
            "valid": r.valid,
            "failed_total": r.failed_total,
            "failures": r.failures.iter().map(DealErrorDto::from).collect::<Vec<_>>(),
            "report_dates": r.report_dates.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            "rejected_total": r.rejected_total,
            "rejected": r.rejected.iter().map(RejectedDto::from).collect::<Vec<_>>(),
            "elapsed_ms": r.elapsed.as_secs_f64() * 1e3,
        })))
    })
    .await
}

fn scenario_list(req: &PortfolioValuationRequest) -> Vec<String> {
    match &req.scenarios {
        Some(list) if !list.is_empty() => list.iter().map(|s| s.trim().to_ascii_uppercase()).collect(),
        _ => {
            let mut seen: Vec<String> = Vec::new();
            for c in req.curve_sets.iter().flat_map(|s| &s.curves) {
                let s = c.scenario.trim().to_ascii_uppercase();
                if !seen.contains(&s) {
                    seen.push(s);
                }
            }
            seen
        }
    }
}

fn build_query(p: &Portfolio, req: PortfolioValuationRequest) -> Result<ValuationQuery> {
    let as_of = parse_date(&req.as_of)?;
    let scenarios = scenario_list(&req);
    if scenarios.is_empty() {
        return Err(Error::new("no scenarios to value"));
    }
    let max_days = p.horizon_days(as_of)?;
    let mut sets = Vec::with_capacity(req.curve_sets.len());
    for set in req.curve_sets {
        let mut curves = Vec::with_capacity(set.curves.len());
        for c in set.curves {
            let compounding = match &c.compounding {
                Some(v) => Compounding::parse(v)?,
                None => Compounding::Continuous,
            };
            let points = c
                .points
                .iter()
                .map(|x| CurvePoint { tenor_days: x.tenor_days, zero_rate: x.zero_rate_percent / 100.0 })
                .collect();
            curves.push(ZeroCurve::new_named(CURVE_TYPE, as_of, &c.currency, &c.scenario, points, compounding)?);
        }
        sets.push(CurveSetTables::new(set.name, curves, &scenarios, max_days)?);
    }
    Ok(ValuationQuery {
        as_of,
        sets,
        scenarios,
        sample: req.sample_ids.into_iter().collect::<HashSet<_>>(),
        only: req.only_ids.map(|v| v.into_iter().collect()),
        keep: req.keep.filter(|k| !k.trim().is_empty()),
    })
}

pub async fn valuations(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PortfolioValuationRequest>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = find(&state, &id)?;
    blocking(move || {
        let q = build_query(&p, req).map_err(invalid)?;
        let out = p.value(&q).map_err(invalid)?;
        let scen = &q.scenarios;
        let as_map = |v: &[f64]| scen.iter().cloned().zip(v.iter().copied()).collect::<BTreeMap<_, _>>();
        let sets: Vec<SetTotalsDto> = q
            .sets
            .iter()
            .zip(&out.totals)
            .map(|(s, t)| SetTotalsDto { name: s.name.clone(), npv: as_map(t) })
            .collect();
        let deals: Vec<DealValuesDto> = out
            .deals
            .iter()
            .map(|d| DealValuesDto {
                deal_id: d.deal_id,
                outstanding: d.outstanding,
                npv: d.npv.iter().map(|v| as_map(v)).collect(),
            })
            .collect();
        Ok(Json(serde_json::json!({
            "as_of": out.as_of.to_string(),
            "valued": out.valued,
            "matured": out.matured,
            "failed_total": out.failed_total,
            "failures": out.failures.iter().map(DealErrorDto::from).collect::<Vec<_>>(),
            "sets": sets,
            "deals": deals,
            "elapsed_ms": out.elapsed.as_secs_f64() * 1e3,
        })))
    })
    .await
}

pub async fn movers(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<MoversQuery>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = find(&state, &id)?;
    let list = p.movers(&q.from, &q.to, q.n.min(1000)).map_err(invalid)?;
    Ok(Json(serde_json::json!(list
        .iter()
        .map(|m| serde_json::json!({"deal_id": m.deal_id, "before": m.before, "after": m.after, "change": m.change}))
        .collect::<Vec<_>>())))
}
