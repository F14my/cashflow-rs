use super::dto::{deal_dto, records_to_table, ErrorDto, ValuationRequest, ValuationResponse};
use crate::application::valuation::{scenario_totals, ValuationService};
use crate::domain::cashflow::CalcConfig;
use crate::domain::curves::{Compounding, CurvePoint, CurveSet, ZeroCurve};
use crate::domain::dates::parse_date;
use crate::domain::error::{Error, Result};
use crate::domain::indicators::IndicatorHistory;
use crate::infrastructure::csv_deals::parse_deal_records;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

const CURVE_TYPE: &str = "API";
const MAX_BODY: usize = 128 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub indicators: Arc<IndicatorHistory>,
    pub threads: usize,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/valuations", post(valuations))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "service": "cashflow-rs", "version": env!("CARGO_PKG_VERSION")}))
}

struct ApiError(StatusCode, &'static str, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(ErrorDto {
                code: self.1,
                message: self.2,
            }),
        )
            .into_response()
    }
}

fn invalid(e: Error) -> ApiError {
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_error",
        e.to_string(),
    )
}

async fn valuations(
    State(state): State<AppState>,
    Json(req): Json<ValuationRequest>,
) -> std::result::Result<Json<ValuationResponse>, ApiError> {
    tokio::task::spawn_blocking(move || value(state, req))
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            )
        })?
        .map(Json)
}

fn build_curves(req: &ValuationRequest) -> Result<CurveSet> {
    let mut curves = Vec::with_capacity(req.curves.len());
    for c in &req.curves {
        let compounding = match &c.compounding {
            Some(v) => Compounding::parse(v)?,
            None => Compounding::Continuous,
        };
        let points = c
            .points
            .iter()
            .map(|p| CurvePoint {
                tenor_days: p.tenor_days,
                zero_rate: p.zero_rate_percent / 100.0,
            })
            .collect();
        curves.push(ZeroCurve::new_named(
            CURVE_TYPE,
            parse_date(&c.curve_date)?,
            &c.currency,
            &c.scenario,
            points,
            compounding,
        )?);
    }
    CurveSet::from_curves(curves)
}

fn value(
    state: AppState,
    req: ValuationRequest,
) -> std::result::Result<ValuationResponse, ApiError> {
    let started = Instant::now();
    if req.deals.is_empty() {
        return Err(invalid(Error::new("deals must not be empty")));
    }
    if req.curves.is_empty() {
        return Err(invalid(Error::new("curves must not be empty")));
    }
    let curves = build_curves(&req).map_err(invalid)?;
    let scenarios: Vec<String> = match &req.scenarios {
        Some(list) if !list.is_empty() => {
            list.iter().map(|s| s.trim().to_ascii_uppercase()).collect()
        }
        _ => {
            let mut seen = Vec::new();
            for c in &req.curves {
                let s = c.scenario.trim().to_ascii_uppercase();
                if !seen.contains(&s) {
                    seen.push(s);
                }
            }
            seen
        }
    };
    let indicators = if req.indicators.is_empty() {
        state.indicators.clone()
    } else {
        let mut merged = (*state.indicators).clone();
        for ind in &req.indicators {
            let points = ind
                .points
                .iter()
                .map(|p| Ok((parse_date(&p.effective_date)?, p.rate_percent / 100.0)))
                .collect::<Result<Vec<_>>>()
                .map_err(invalid)?;
            merged.insert(&ind.code, points).map_err(invalid)?;
        }
        Arc::new(merged)
    };
    let (columns, rows) = records_to_table(&req.deals);
    let deals = parse_deal_records(&columns, &rows).map_err(invalid)?;
    let cfg = CalcConfig {
        act_act_as_365: req.act_act_as_365,
    };
    let service = ValuationService::new(indicators.as_ref(), &curves, cfg, state.threads);
    let valuations = service.value(&deals, CURVE_TYPE, &scenarios);
    let totals = scenario_totals(&valuations, &scenarios);
    Ok(ValuationResponse {
        scenarios: totals.iter().map(Into::into).collect(),
        deals: valuations
            .iter()
            .map(|v| deal_dto(v, &scenarios, req.include_cashflows))
            .collect(),
        deal_count: deals.len(),
        elapsed_ms: started.elapsed().as_secs_f64() * 1e3,
    })
}
