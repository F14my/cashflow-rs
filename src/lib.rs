pub mod cashflow;
pub mod curves;
pub mod dates;
pub mod discounting;
pub mod error;
pub mod indicators;
pub mod model;
pub mod parser;
pub mod principal;
pub mod rates;

pub use cashflow::{build_cashflows, CalcConfig};
pub use error::{Error, Result};
pub use model::{CashFlow, Deal, DiscountedCashFlow};

use indicators::IndicatorSource;

#[derive(Clone, Debug, Default)]
pub struct DealSummary {
    pub deal_id: i64,
    pub principal: f64,
    pub interest: f64,
    pub total_cf: f64,
    pub pv_principal: f64,
    pub pv_interest: f64,
    pub npv: f64,
}

#[derive(Debug)]
pub struct DealResult {
    pub deal_id: i64,
    pub cashflows: Vec<CashFlow>,
    pub summary: DealSummary,
}

pub fn calculate_deal(
    deal: &Deal,
    indicators: &dyn IndicatorSource,
    cfg: &CalcConfig,
) -> Result<DealResult> {
    let cashflows = build_cashflows(deal, indicators, cfg)
        .map_err(|e| e.context(format!("deal_id={}", deal.deal_id)))?;
    let mut summary = DealSummary {
        deal_id: deal.deal_id,
        ..Default::default()
    };
    for c in &cashflows {
        summary.principal += c.principal_cf;
        summary.interest += c.interest_cf;
        summary.total_cf += c.total();
    }
    Ok(DealResult {
        deal_id: deal.deal_id,
        cashflows,
        summary,
    })
}

pub fn calculate_portfolio(
    deals: &[Deal],
    indicators: &dyn IndicatorSource,
    cfg: &CalcConfig,
    threads: usize,
) -> Vec<Result<DealResult>> {
    let threads = threads.max(1).min(deals.len().max(1));
    if threads == 1 {
        return deals
            .iter()
            .map(|d| calculate_deal(d, indicators, cfg))
            .collect();
    }
    let chunk = deals.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = deals
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|d| calculate_deal(d, indicators, cfg))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker panicked"))
            .collect()
    })
}
