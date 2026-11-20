use crate::application::ports::DiscountCurves;
use crate::domain::cashflow::CalcConfig;
use crate::domain::error::Result;
use crate::domain::indicators::IndicatorSource;
use crate::domain::model::Deal;
use crate::domain::valuation::{
    calculate_deal, discount_deal, DealResult, DealSummary, DiscountedDeal,
};

/// Valuation of one deal under every requested scenario.
#[derive(Debug)]
pub struct DealValuation {
    pub deal_id: i64,
    /// One entry per requested scenario, in request order.
    pub scenarios: Vec<Result<DiscountedDeal>>,
}

/// Portfolio totals of one scenario over the deals that were valued successfully.
#[derive(Clone, Debug, Default)]
pub struct ScenarioTotal {
    pub scenario: String,
    pub total: DealSummary,
    pub ok: usize,
    pub failed: usize,
}

pub struct ValuationService<'a> {
    indicators: &'a dyn IndicatorSource,
    curves: &'a dyn DiscountCurves,
    cfg: CalcConfig,
    threads: usize,
}

impl<'a> ValuationService<'a> {
    pub fn new(
        indicators: &'a dyn IndicatorSource,
        curves: &'a dyn DiscountCurves,
        cfg: CalcConfig,
        threads: usize,
    ) -> Self {
        Self {
            indicators,
            curves,
            cfg,
            threads: threads.max(1),
        }
    }

    /// Builds schedules once per deal and discounts them by every scenario curve.
    /// Deals are processed in parallel; output order matches input order.
    pub fn value(
        &self,
        deals: &[Deal],
        curve_type: &str,
        scenarios: &[String],
    ) -> Vec<DealValuation> {
        let threads = self.threads.min(deals.len().max(1));
        let one = |deal: &Deal| self.value_deal(deal, curve_type, scenarios);
        if threads == 1 {
            return deals.iter().map(one).collect();
        }
        let chunk = deals.len().div_ceil(threads);
        std::thread::scope(|s| {
            let handles: Vec<_> = deals
                .chunks(chunk)
                .map(|part| s.spawn(move || part.iter().map(one).collect::<Vec<_>>()))
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("valuation worker panicked"))
                .collect()
        })
    }

    fn value_deal(&self, deal: &Deal, curve_type: &str, scenarios: &[String]) -> DealValuation {
        let schedule: Result<DealResult> = calculate_deal(deal, self.indicators, &self.cfg);
        let scenarios = scenarios
            .iter()
            .map(|scenario| {
                let schedule = schedule.as_ref().map_err(Clone::clone)?;
                let curve = self
                    .curves
                    .curve(curve_type, &deal.currency, deal.report_date, scenario)
                    .map_err(|e| e.context(format!("deal_id={}", deal.deal_id)))?;
                discount_deal(deal, schedule, curve)
            })
            .collect();
        DealValuation {
            deal_id: deal.deal_id,
            scenarios,
        }
    }
}

/// Sums successful deal values per scenario.
pub fn scenario_totals(valuations: &[DealValuation], scenarios: &[String]) -> Vec<ScenarioTotal> {
    scenarios
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut total = ScenarioTotal {
                scenario: name.clone(),
                ..Default::default()
            };
            for v in valuations {
                match &v.scenarios[i] {
                    Ok(d) => {
                        total.total.add(&d.summary);
                        total.ok += 1;
                    }
                    Err(_) => total.failed += 1,
                }
            }
            total
        })
        .collect()
}
