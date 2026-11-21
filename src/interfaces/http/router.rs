use super::dto::{
    deal_dto, records_to_table, CurveDto, CurvePointDto, ErrorDto, IndicatorDto, PrepaymentDto,
    ValuationRequest, ValuationResponse,
};
use super::portfolio;
use crate::application::portfolio::PortfolioStore;
use crate::application::valuation::{scenario_totals, ValuationService};
use crate::domain::cashflow::CalcConfig;
use crate::domain::curves::{Compounding, CurvePoint, CurveSet, ZeroCurve};
use crate::domain::dates::parse_date;
use crate::domain::error::{Error, Result};
use crate::domain::indicators::IndicatorHistory;
use crate::domain::prepayment::PrepaymentPolicy;
use crate::infrastructure::csv_deals::parse_deal_records;
use crate::infrastructure::curve_service::CurveService;
use std::collections::BTreeSet;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

const CURVE_TYPE: &str = "API";
const MAX_BODY: usize = 256 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub indicators: Arc<IndicatorHistory>,
    pub threads: usize,
    pub portfolios: Arc<PortfolioStore>,
    /// Prepayment used when a request does not say (`None`: contractual schedules).
    pub prepayment: Option<Arc<PrepaymentPolicy>>,
    /// Curve service for requests that ask for its curves.
    pub curve_service: Option<Arc<CurveService>>,
}

/// Prepayment policy of a request: its own settings, or the server default.
pub fn prepayment_policy(
    state: &AppState,
    req: Option<&PrepaymentDto>,
) -> std::result::Result<Option<Arc<PrepaymentPolicy>>, ApiError> {
    match req {
        None => Ok(state.prepayment.clone()),
        Some(p) if !p.enabled => Ok(None),
        Some(p) => {
            let policy = PrepaymentPolicy::standard().with_rates(p.rates.iter().map(|(k, v)| (k, *v)));
            policy.validate().map_err(invalid)?;
            Ok(Some(Arc::new(policy)))
        }
    }
}

pub fn curve_service(state: &AppState) -> std::result::Result<&CurveService, ApiError> {
    state.curve_service.as_deref().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "curve_service_unconfigured",
            "the calculator has no curve service (set CFCALC_CURVES_URL)".into(),
        )
    })
}

pub fn upstream(e: Error) -> ApiError {
    ApiError(StatusCode::BAD_GATEWAY, "curve_service_error", e.to_string())
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/valuations", post(valuations))
        .route("/api/v1/curves/{date}", get(curves_on))
        .route("/api/v1/prepayment", get(prepayment_defaults))
        .route("/api/v1/portfolios", post(portfolio::create))
        .route(
            "/api/v1/portfolios/{id}",
            get(portfolio::info).delete(portfolio::remove),
        )
        .route("/api/v1/portfolios/{id}/deals", post(portfolio::upload))
        .route("/api/v1/portfolios/{id}/seal", post(portfolio::seal))
        .route(
            "/api/v1/portfolios/{id}/valuations",
            post(portfolio::valuations),
        )
        .route("/api/v1/portfolios/{id}/movers", get(portfolio::movers))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "cashflow-rs",
        "version": env!("CARGO_PKG_VERSION"),
        "curve_service": state.curve_service.as_ref().map(|c| c.base_url().to_string()),
        "prepayment_default": state.prepayment.is_some(),
    }))
}

/// Curves of the curve service for a date, in the calculator's own format.
async fn curves_on(
    State(state): State<AppState>,
    axum::extract::Path(date): axum::extract::Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    parse_date(&date).map_err(invalid)?;
    let service = curve_service(&state)?.clone();
    let (trade_date, curves) = tokio::task::spawn_blocking(move || service.curves(&date))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e.to_string()))?
        .map_err(upstream)?;
    Ok(Json(json!({
        "trade_date": trade_date,
        "compounding": "annual",
        "curves": curves.iter().map(|c| json!({
            "scenario": c.scenario,
            "points": c.points.iter().map(|(d, r)| json!({"tenor_days": d, "zero_rate_percent": r})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })))
}

/// Standard prepayment rates and whether they apply by default.
async fn prepayment_defaults(State(state): State<AppState>) -> Json<serde_json::Value> {
    let policy = state.prepayment.as_deref().cloned().unwrap_or_else(PrepaymentPolicy::standard);
    Json(json!({"default_enabled": state.prepayment.is_some(), "rates": policy.rates()}))
}

pub struct ApiError(pub StatusCode, pub &'static str, pub String);

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

pub fn invalid(e: Error) -> ApiError {
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
    mut req: ValuationRequest,
) -> std::result::Result<ValuationResponse, ApiError> {
    let started = Instant::now();
    if req.deals.is_empty() {
        return Err(invalid(Error::new("deals must not be empty")));
    }
    let indicators = merged_indicators(&state, &req.indicators)?;
    let (columns, rows) = records_to_table(&req.deals);
    let deals = parse_deal_records(&columns, &rows).map_err(invalid)?;
    if req.curves_from_service {
        if !req.curves.is_empty() {
            return Err(invalid(Error::new("pass either curves or curves_from_service, not both")));
        }
        let service = curve_service(&state)?;
        let dates: BTreeSet<_> = deals.iter().map(|d| d.report_date).collect();
        for date in dates {
            let (_, curves) = service.curves(&date.to_string()).map_err(upstream)?;
            for c in curves {
                req.curves.push(CurveDto {
                    scenario: c.scenario,
                    curve_date: date.to_string(),
                    currency: "RUB".into(),
                    compounding: Some("annual".into()),
                    points: c
                        .points
                        .into_iter()
                        .map(|(tenor_days, zero_rate_percent)| CurvePointDto { tenor_days, zero_rate_percent })
                        .collect(),
                });
            }
        }
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
    let cfg = CalcConfig {
        act_act_as_365: req.act_act_as_365,
        prepayment: prepayment_policy(&state, req.prepayment.as_ref())?,
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

/// Server indicator histories with the request's series merged over them.
pub fn merged_indicators(
    state: &AppState,
    extra: &[IndicatorDto],
) -> std::result::Result<Arc<IndicatorHistory>, ApiError> {
    if extra.is_empty() {
        return Ok(state.indicators.clone());
    }
    let mut merged = (*state.indicators).clone();
    for ind in extra {
        let points = ind
            .points
            .iter()
            .map(|p| Ok((parse_date(&p.effective_date)?, p.rate_percent / 100.0)))
            .collect::<Result<Vec<_>>>()
            .map_err(invalid)?;
        merged.insert(&ind.code, points).map_err(invalid)?;
    }
    Ok(Arc::new(merged))
}
