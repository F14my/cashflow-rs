use crate::domain::curves::{CurveSet, ZeroCurve};
use crate::domain::dates::Date;
use crate::domain::error::Result;

/// Source of discount curves required by the valuation use case.
pub trait DiscountCurves: Sync {
    fn curve(
        &self,
        curve_type: &str,
        currency: &str,
        report_date: Date,
        scenario: &str,
    ) -> Result<&ZeroCurve>;
}

impl DiscountCurves for CurveSet {
    fn curve(
        &self,
        curve_type: &str,
        currency: &str,
        report_date: Date,
        scenario: &str,
    ) -> Result<&ZeroCurve> {
        self.curve_for(curve_type, currency, report_date, scenario)
    }
}
