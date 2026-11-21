use crate::domain::cashflow::{build_cashflows, CalcConfig};
use crate::domain::curves::ZeroCurve;
use crate::domain::discounting::discount_cashflow;
use crate::domain::error::Result;
use crate::domain::indicators::IndicatorSource;
use crate::domain::model::{CashFlow, Deal, DiscountedCashFlow};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DealSummary {
    pub deal_id: i64,
    pub principal: f64,
    pub interest: f64,
    pub subsidy: f64,
    pub prepayment: f64,
    pub total_cf: f64,
    pub pv_principal: f64,
    pub pv_interest: f64,
    pub pv_subsidy: f64,
    pub pv_prepayment: f64,
    pub npv: f64,
}

impl DealSummary {
    pub fn add(&mut self, other: &DealSummary) {
        self.principal += other.principal;
        self.interest += other.interest;
        self.subsidy += other.subsidy;
        self.prepayment += other.prepayment;
        self.total_cf += other.total_cf;
        self.pv_principal += other.pv_principal;
        self.pv_interest += other.pv_interest;
        self.pv_subsidy += other.pv_subsidy;
        self.pv_prepayment += other.pv_prepayment;
        self.npv += other.npv;
    }
}

#[derive(Clone, Debug)]
pub struct DealResult {
    pub deal_id: i64,
    pub cashflows: Vec<CashFlow>,
    pub summary: DealSummary,
}

#[derive(Clone, Debug)]
pub struct DiscountedDeal {
    pub summary: DealSummary,
    pub cashflows: Vec<DiscountedCashFlow>,
}

/// Builds the undiscounted schedule of one deal. The schedule does not depend
/// on the discount curve, so it is computed once and discounted per scenario.
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
        summary.subsidy += c.subsidy_cf;
        summary.prepayment += c.prepayment_cf;
        summary.total_cf += c.total();
    }
    Ok(DealResult {
        deal_id: deal.deal_id,
        cashflows,
        summary,
    })
}

/// Discounts an already built schedule with one zero curve and sums PV/NPV.
pub fn discount_deal(
    deal: &Deal,
    result: &DealResult,
    curve: &ZeroCurve,
) -> Result<DiscountedDeal> {
    let mut summary = result.summary.clone();
    let mut cashflows = Vec::with_capacity(result.cashflows.len());
    for cf in &result.cashflows {
        let d = discount_cashflow(cf.clone(), deal.report_date, curve)
            .map_err(|e| e.context(format!("deal_id={}", deal.deal_id)))?;
        summary.pv_principal += d.discounted_principal_cf;
        summary.pv_interest += d.discounted_interest_cf;
        summary.pv_subsidy += d.discounted_subsidy_cf;
        summary.pv_prepayment += d.discounted_prepayment_cf;
        summary.npv += d.discounted_total_cf;
        cashflows.push(d);
    }
    Ok(DiscountedDeal { summary, cashflows })
}
