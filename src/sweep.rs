//! **Sweeps**: vary one input across a range of values and find where the
//! plan stops (or starts) working.
//!
//! A sweep is a parameter and an ascending list of values:
//!
//! ```json
//! { "parameter": "retirement_shift", "values": [-4, -2, 0, 2, 4] }
//! ```
//!
//! Each value becomes one [`Override`] on a copy of the plan; the copies are
//! projected concurrently through [`project_all`]; and the result is a
//! compact series (one point per value) plus the **breakpoints**: each pair
//! of neighboring values where the plan goes from lasting through the final
//! year to running short, or back.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::batch::project_all;
use crate::error::Error;
use crate::overrides::{Override, apply_overrides};
use crate::plan::Plan;
use crate::projector::Projector;
use crate::summary::Summary;

/// Most values in one sweep.
pub const MAX_VALUES: usize = 25;

/// The input a sweep varies. Values use the plan's own units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Parameter {
    /// The first drawdown year's withdrawal, in cents (plans without
    /// itemized expenses).
    AnnualWithdrawal,
    /// Years to retire later (+) or earlier (-), for everyone. The plan's
    /// final year stays fixed.
    RetirementShift,
    /// Basis points added to every account's return (-100 = 1 point lower).
    ReturnAdjustment,
    /// Drawdown spending as a percentage of the plan's (90 = 10% less).
    ExpenseScale,
}

/// What to vary and which values to try.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sweep {
    pub parameter: Parameter,
    /// 1 to 25 values in strictly ascending order.
    pub values: Vec<i64>,
}

impl Sweep {
    /// Parses and checks a sweep from a tool call's JSON. Problems name the
    /// field (`values[3]`), never the value.
    pub fn from_value(value: serde_json::Value) -> Result<Sweep, Error> {
        let sweep: Sweep = serde_path_to_error::deserialize(value).map_err(|err| {
            let path = err.path().to_string();
            match Error::from_json_error(&path, err.inner()) {
                Error::PlanShape { location, problem } => Error::SweepShape {
                    location: location.trim_matches('`').to_string(),
                    problem,
                },
                other => other,
            }
        })?;
        sweep.check()?;
        Ok(sweep)
    }

    /// Count and order rules. Range rules for each value come from the
    /// matching override when it's applied.
    fn check(&self) -> Result<(), Error> {
        let shape = |location: &str, problem: String| Error::SweepShape {
            location: location.to_string(),
            problem,
        };
        if self.values.is_empty() || self.values.len() > MAX_VALUES {
            return Err(shape(
                "values",
                format!("give between 1 and {MAX_VALUES} values"),
            ));
        }
        // `windows(2)` yields every overlapping pair of neighbors: [a, b],
        // [b, c], [c, d], ... It borrows slices of the list; nothing is
        // copied. `position` finds the first pair that's out of order.
        if let Some(index) = self.values.windows(2).position(|pair| pair[0] >= pair[1]) {
            return Err(shape(
                &format!("values[{}]", index + 1),
                "values must be in strictly ascending order".to_string(),
            ));
        }
        Ok(())
    }

    /// The override for each value, in order.
    ///
    /// `impl Iterator<Item = ...>` in *return* position means "some iterator
    /// of these items; the exact type is the compiler's business". The chain
    /// of `.iter().map(...)` has an unnameable closure type, so this is the
    /// only practical way to return it without collecting into a `Vec`.
    /// Nothing runs until the caller consumes it: iterators are lazy, like
    /// Python generators. `+ '_` says the iterator borrows from `self`.
    fn overrides(&self) -> impl Iterator<Item = Result<Override, String>> + '_ {
        self.values
            .iter()
            .map(move |&value| override_for(self.parameter, value))
    }
}

/// Builds the override that sets `parameter` to `value`.
fn override_for(parameter: Parameter, value: i64) -> Result<Override, String> {
    // Every parameter except the withdrawal is an `i32` in its override.
    // `i32::try_from` fails instead of silently truncating, unlike `as i32`.
    let small = || i32::try_from(value).map_err(|_| "value is out of range".to_string());
    Ok(match parameter {
        Parameter::AnnualWithdrawal => {
            if value < 0 {
                return Err("annual withdrawal must be at least 0".to_string());
            }
            Override::SetAnnualWithdrawal {
                annual_withdrawal_cents: value,
            }
        }
        Parameter::RetirementShift => Override::ShiftRetirement {
            years: small()?,
            person_id: None,
        },
        Parameter::ReturnAdjustment => Override::AdjustReturns {
            basis_points: small()?,
            drawdown_only: false,
        },
        Parameter::ExpenseScale => Override::ScaleExpenses { percent: small()? },
    })
}

/// The swept series and where the outcome changes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepResult {
    pub parameter: Parameter,
    /// One point per value, in order.
    pub points: Vec<Point>,
    /// Each neighboring pair of values where the plan goes from lasting to
    /// running short, or back. Empty when the outcome never changes.
    pub breakpoints: Vec<Breakpoint>,
}

/// One value's outcome: the headline numbers of its summary, kept compact
/// because a sweep can have 25 of them. Whole dollars.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub value: i64,
    pub lasts_through_final_year: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depletion_year: Option<i32>,
    pub years_short: usize,
    pub ending_balance: i64,
    pub lowest_balance: i64,
    pub total_unfunded: i64,
}

impl Point {
    fn new(value: i64, summary: &Summary) -> Point {
        Point {
            value,
            lasts_through_final_year: summary.lasts_through_final_year,
            depletion_year: summary.depletion_year,
            years_short: summary.years_short,
            ending_balance: summary.ending_balance,
            lowest_balance: summary.lowest_balance.amount,
            total_unfunded: summary.total_unfunded,
        }
    }
}

/// Two neighboring values with different outcomes. The real threshold lies
/// somewhere between them; a finer sweep can narrow it down.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    /// The value where the plan lasts through the final year.
    pub lasts_at: i64,
    /// Its neighbor, where the plan runs short.
    pub short_at: i64,
}

/// Every neighboring pair of points whose outcomes differ.
///
/// Usually there's at most one, since more spending or less saving only makes
/// things worse. But accounts locked until a later year can make outcomes
/// non-monotonic, so every change is reported rather than only the first.
pub fn breakpoints(points: &[Point]) -> Vec<Breakpoint> {
    points
        .windows(2)
        .filter_map(|pair| {
            // Destructuring a two-element slice pattern: `[a, b]` names both
            // elements. The `else` branch handles any other length, which
            // `windows(2)` never produces but the compiler can't know that.
            let [before, after] = pair else {
                return None;
            };
            match (
                before.lasts_through_final_year,
                after.lasts_through_final_year,
            ) {
                (true, false) => Some(Breakpoint {
                    lasts_at: before.value,
                    short_at: after.value,
                }),
                (false, true) => Some(Breakpoint {
                    lasts_at: after.value,
                    short_at: before.value,
                }),
                _ => None,
            }
        })
        .collect()
}

/// Runs a sweep: one projection per value, concurrently.
///
/// Every value must apply and every projection must succeed: a series with
/// holes would hide breakpoints. A value that doesn't apply (retiring before
/// year 0, say) is reported by its position, `values[3]`.
pub async fn run_sweep<P: Projector + 'static>(
    projector: Arc<P>,
    plan: &Plan,
    sweep: &Sweep,
) -> Result<SweepResult, Error> {
    plan.check()?;
    sweep.check()?;
    if sweep.parameter == Parameter::AnnualWithdrawal && !plan.expenses.is_empty() {
        return Err(Error::SweepShape {
            location: "parameter".to_string(),
            problem: "this plan itemizes expenses; sweep expense_scale instead".to_string(),
        });
    }

    // Turn each value into a changed plan, stopping at the first that fails.
    // Collecting an iterator of `Result`s into `Result<Vec<_>, _>` does
    // exactly that: `Ok(vec)` if all succeed, otherwise the first `Err`.
    let plans: Vec<Plan> = sweep
        .overrides()
        .enumerate()
        .map(|(index, change)| {
            let at_value = |problem: String| Error::SweepShape {
                location: format!("values[{index}]"),
                problem,
            };
            let change = change.map_err(at_value)?;
            apply_overrides(plan, std::slice::from_ref(&change)).map_err(|err| match err {
                Error::InvalidOverride { problem, .. } => at_value(problem),
                other => other,
            })
        })
        .collect::<Result<Vec<Plan>, Error>>()?;

    let summaries = project_all(projector, plans.into_iter().map(Ok).collect()).await;

    let points = sweep
        .values
        .iter()
        .zip(summaries)
        .map(|(&value, summary)| Ok(Point::new(value, &summary?)))
        .collect::<Result<Vec<Point>, Error>>()?;

    Ok(SweepResult {
        parameter: sweep.parameter,
        breakpoints: breakpoints(&points),
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    const BASIC: &str = include_str!("../tests/fixtures/plan-basic.json");
    const HOUSEHOLD: &str = include_str!("../tests/fixtures/plan-household.json");
    const PROJECTION_BASIC: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn plan() -> Plan {
        Plan::from_json_checked(BASIC).unwrap()
    }

    fn sweep(json: &str) -> Sweep {
        Sweep::from_value(serde_json::from_str(json).unwrap()).unwrap()
    }

    fn sweep_error(json: &str) -> String {
        Sweep::from_value(serde_json::from_str(json).unwrap())
            .unwrap_err()
            .to_string()
    }

    /// A projection that lasts (no shortfall, ending balance `ending`
    /// dollars) or runs short, based on the basic fixture, which runs short.
    fn outcome(lasts: bool, ending: i64) -> Projection {
        let mut projection: Projection = serde_json::from_str(PROJECTION_BASIC).unwrap();
        if lasts {
            for year in &mut projection.totals {
                year.unfunded_withdrawal_cents = 0;
            }
        }
        projection.totals.last_mut().unwrap().balance_cents = ending * 100;
        projection
    }

    /// A fake that lasts while the plan's first-year withdrawal is at most
    /// `limit` cents, with an ending balance that shrinks as it grows.
    fn withdrawal_fake(limit: i64) -> Arc<FakeProjector> {
        Arc::new(FakeProjector::from_fn(move |plan| {
            let withdrawal = plan.annual_withdrawal_cents;
            Ok(outcome(
                withdrawal <= limit,
                (limit - withdrawal).max(0) / 100,
            ))
        }))
    }

    fn lasts(points: &[Point]) -> Vec<bool> {
        points.iter().map(|p| p.lasts_through_final_year).collect()
    }

    #[tokio::test]
    async fn finds_the_breakpoint_in_the_middle() {
        let request = sweep(
            r#"{"parameter": "annual_withdrawal", "values": [4000000, 5000000, 6000000, 7000000]}"#,
        );

        let result = run_sweep(withdrawal_fake(5_500_000), &plan(), &request)
            .await
            .unwrap();

        assert_eq!(result.parameter, Parameter::AnnualWithdrawal);
        assert_eq!(lasts(&result.points), [true, true, false, false]);
        assert_eq!(
            result.breakpoints,
            [Breakpoint {
                lasts_at: 5_000_000,
                short_at: 6_000_000
            }]
        );
        assert_eq!(result.points[0].value, 4_000_000);
        assert_eq!(result.points[0].ending_balance, 15_000);
        assert_eq!(result.points[2].depletion_year, Some(36));
    }

    #[tokio::test]
    async fn short_at_the_first_value_or_lasting_throughout_has_no_breakpoint() {
        let request = sweep(r#"{"parameter": "annual_withdrawal", "values": [6000000, 7000000]}"#);
        let result = run_sweep(withdrawal_fake(5_000_000), &plan(), &request)
            .await
            .unwrap();
        assert_eq!(lasts(&result.points), [false, false]);
        assert!(result.breakpoints.is_empty());

        let result = run_sweep(withdrawal_fake(9_000_000), &plan(), &request)
            .await
            .unwrap();
        assert_eq!(lasts(&result.points), [true, true]);
        assert!(result.breakpoints.is_empty());
    }

    #[tokio::test]
    async fn a_breakpoint_where_the_plan_starts_lasting() {
        // Retiring later helps: short when early, lasting when late.
        let fake = Arc::new(FakeProjector::from_fn(|plan| {
            Ok(outcome(plan.saving_years >= 11, 0))
        }));
        let request = sweep(r#"{"parameter": "retirement_shift", "values": [-2, 0, 2]}"#);

        let result = run_sweep(Arc::clone(&fake), &plan(), &request)
            .await
            .unwrap();

        assert_eq!(lasts(&result.points), [false, false, true]);
        assert_eq!(
            result.breakpoints,
            [Breakpoint {
                lasts_at: 2,
                short_at: 0
            }]
        );
        // Each value was applied as its own override: saving years 8, 10, 12.
        let saving: Vec<i32> = fake.received().iter().map(|p| p.saving_years).collect();
        assert_eq!(saving.len(), 3);
        assert!(saving.contains(&8) && saving.contains(&10) && saving.contains(&12));
    }

    #[test]
    fn every_change_of_outcome_is_a_breakpoint() {
        let point = |value, lasts| Point {
            value,
            lasts_through_final_year: lasts,
            depletion_year: None,
            years_short: 0,
            ending_balance: 0,
            lowest_balance: 0,
            total_unfunded: 0,
        };
        let points = [
            point(1, true),
            point(2, false),
            point(3, true),
            point(4, true),
        ];
        assert_eq!(
            breakpoints(&points),
            [
                Breakpoint {
                    lasts_at: 1,
                    short_at: 2
                },
                Breakpoint {
                    lasts_at: 3,
                    short_at: 2
                },
            ]
        );
        assert!(breakpoints(&points[..1]).is_empty());
    }

    #[tokio::test]
    async fn each_parameter_maps_to_its_override() {
        let fake = Arc::new(FakeProjector::from_fn(|_| Ok(outcome(true, 0))));
        let household = Plan::from_json_checked(HOUSEHOLD).unwrap();

        run_sweep(
            Arc::clone(&fake),
            &plan(),
            &sweep(r#"{"parameter": "return_adjustment", "values": [-100]}"#),
        )
        .await
        .unwrap();
        run_sweep(
            Arc::clone(&fake),
            &household,
            &sweep(r#"{"parameter": "expense_scale", "values": [50]}"#),
        )
        .await
        .unwrap();

        let received = fake.received();
        assert_eq!(received[0].items[0].annual_return_rate_basis_points, 600);
        assert_eq!(received[1].expenses[0].annual_amount_cents, 2_750_000);
    }

    #[test]
    fn values_are_checked() {
        assert_eq!(
            sweep_error(r#"{"parameter": "expense_scale", "values": []}"#),
            "sweep problem at `values`: give between 1 and 25 values"
        );
        let many: Vec<i64> = (0..26).collect();
        assert_eq!(
            sweep_error(&format!(
                r#"{{"parameter": "expense_scale", "values": {many:?}}}"#
            )),
            "sweep problem at `values`: give between 1 and 25 values"
        );
        assert_eq!(
            sweep_error(r#"{"parameter": "expense_scale", "values": [80, 90, 90]}"#),
            "sweep problem at `values[2]`: values must be in strictly ascending order"
        );
        assert!(
            sweep_error(r#"{"parameter": "spending", "values": [1]}"#)
                .starts_with("sweep problem at `parameter`: unknown variant `spending`")
        );
        let message = sweep_error(r#"{"parameter": "expense_scale", "values": [1.5]}"#);
        assert_eq!(
            message,
            "sweep problem at `values[0]`: invalid type, expected a whole number (cents or basis points, no decimals)"
        );
    }

    #[tokio::test]
    async fn values_must_be_valid_for_the_parameter_and_plan() {
        let fake = Arc::new(FakeProjector::from_fn(|_| Ok(outcome(true, 0))));
        let error = |json: &str| {
            let fake = Arc::clone(&fake);
            let request = sweep(json);
            async move {
                run_sweep(fake, &plan(), &request)
                    .await
                    .unwrap_err()
                    .to_string()
            }
        };

        assert_eq!(
            error(r#"{"parameter": "retirement_shift", "values": [-11, 0]}"#).await,
            "sweep problem at `values[0]`: the changed plan breaks a rule: savingYears must be between 0 and 75"
        );
        assert_eq!(
            error(r#"{"parameter": "return_adjustment", "values": [0, 3000000000]}"#).await,
            "sweep problem at `values[1]`: value is out of range"
        );
        assert_eq!(
            error(r#"{"parameter": "annual_withdrawal", "values": [-1, 0]}"#).await,
            "sweep problem at `values[0]`: annual withdrawal must be at least 0"
        );
        assert_eq!(
            error(r#"{"parameter": "expense_scale", "values": [100, 600]}"#).await,
            "sweep problem at `values[1]`: percent must be between 0 and 500"
        );
        let household = Plan::from_json_checked(HOUSEHOLD).unwrap();
        let request = sweep(r#"{"parameter": "annual_withdrawal", "values": [1]}"#);
        assert_eq!(
            run_sweep(Arc::clone(&fake), &household, &request)
                .await
                .unwrap_err()
                .to_string(),
            "sweep problem at `parameter`: this plan itemizes expenses; sweep expense_scale instead"
        );
        assert_eq!(
            error(r#"{"parameter": "expense_scale", "values": [100, 600]}"#).await,
            "sweep problem at `values[1]`: percent must be between 0 and 500"
        );
        // Nothing is projected when a value is invalid.
        assert!(fake.received().is_empty());
    }

    #[tokio::test]
    async fn a_failed_projection_fails_the_sweep() {
        let fake = Arc::new(FakeProjector::from_fn(|plan| {
            if plan.annual_withdrawal_cents == 2 {
                Err(Error::Auth)
            } else {
                Ok(outcome(true, 0))
            }
        }));
        let request = sweep(r#"{"parameter": "annual_withdrawal", "values": [1, 2, 3]}"#);
        let result = run_sweep(fake, &plan(), &request).await;
        assert!(matches!(result, Err(Error::Auth)), "{result:?}");
    }
}
