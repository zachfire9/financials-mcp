//! **Finding the most a plan can spend.**
//!
//! "What's the most we can spend each year and still have $200k left?" has
//! one right answer for a given plan, because financials-api is
//! deterministic: the same plan always gives the same projection. So instead
//! of the model guessing amounts, [`find_max_spending`] searches for the
//! answer:
//!
//! 1. Check that the plan works at all with the least spending allowed: $0,
//!    or 1 cent where financials-api requires a withdrawal. If it doesn't, no
//!    amount will.
//! 2. Start from the plan's current spending and double it until a projection
//!    fails, which brackets the answer between a passing and a failing amount.
//! 3. Binary search between them until they're within [`PRECISION_CENTS`].
//!
//! The whole search is capped at [`MAX_PROJECTIONS`] projections.
//!
//! **What "spending" means:** for a plan with one withdrawal amount, it's
//! `annualWithdrawalCents`. For a plan with itemized expenses, it's the sum of
//! the expenses' annual amounts, and every expense is scaled by the same
//! factor. The gap-year withdrawal and the mortgage stay as they are.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::plan::Plan;
use crate::projection::{Phase, Projection};
use crate::projector::Projector;
use crate::summary::{Summary, dollars, summarize};

/// Most projections one search may run.
pub const MAX_PROJECTIONS: u32 = 25;

/// The search stops when the passing and failing amounts are this close:
/// $100, in cents.
pub const PRECISION_CENTS: i64 = 10_000;

/// The largest annual spending the search tries: $10 million, in cents.
pub const MAX_SPENDING_CENTS: i64 = 1_000_000_000;

/// Where the search starts when the plan has no spending yet: $50,000.
const DEFAULT_START_CENTS: i64 = 5_000_000;

/// What the spending has to satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Constraints {
    /// The balance must stay at or above this, in cents, in every year after
    /// the first retirement through `throughYear`. Default 0.
    #[serde(default)]
    pub min_balance_cents: i64,
    /// The last year the constraints apply to (1 = first projected year).
    /// Default: the plan's final year.
    #[serde(default)]
    pub through_year: Option<i32>,
}

impl Constraints {
    /// Parses constraints from a tool call's JSON (`null` means defaults).
    pub fn from_value(value: serde_json::Value) -> Result<Constraints, Error> {
        if value.is_null() {
            return Ok(Constraints::default());
        }
        serde_path_to_error::deserialize(value).map_err(|err| {
            let path = err.path().to_string();
            match Error::from_json_error(&path, err.inner()) {
                Error::PlanShape { problem, .. } => Error::ConstraintShape {
                    // `path` is a field name like `minBalanceCents`, or `.`
                    // for the constraints object itself.
                    location: if path == "." {
                        "constraints".to_string()
                    } else {
                        format!("constraints.{path}")
                    },
                    problem,
                },
                other => other,
            }
        })
    }
}

/// How the search turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The largest passing amount was found, to within `precision`.
    Found,
    /// The constraints fail even with no spending (or 1 cent, where a
    /// withdrawal is required).
    FailsAtZero,
    /// No failing amount was found: the plan passes at the search's largest
    /// amount, or the projection budget ran out while doubling.
    NoUpperLimit,
}

/// The result of a search. Amounts are whole dollars unless named `Cents`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaxSpending {
    pub outcome: Outcome,
    /// The largest annual spending found that meets the constraints, in
    /// cents, ready to use in a plan or an override. `None` for
    /// `fails_at_zero`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annual_spending_cents: Option<i64>,
    /// The same, in whole dollars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annual_spending: Option<i64>,
    /// Itemized plans: the spending as a share of the plan's current
    /// expenses, in basis points (9250 = 92.5%).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_basis_points: Option<i64>,
    /// Itemized plans: each expense at the amount found.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expenses: Vec<ScaledExpense>,
    /// The gap between the amount found and the smallest amount that failed:
    /// the true maximum is less than `annualSpending + precision`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<i64>,
    pub projections_used: u32,
    pub min_balance: i64,
    pub through_year: i32,
    /// The projection at the amount found (or at $0 for `fails_at_zero`).
    pub summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaledExpense {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    pub annual_amount_cents: i64,
}

/// Which number the search changes.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// Set `annualWithdrawalCents` directly.
    Withdrawal,
    /// Scale every itemized expense so their sum is the target.
    Itemized { current_total: i64 },
}

/// The state of one search: what to change, how to judge it, and how many
/// projections it has used.
///
/// The `<'a, P>` parameters say a `Search` borrows a projector and a plan
/// for lifetime `'a`: it can't outlive either. A struct holding references
/// must name their lifetimes; Rust checks every use against them.
struct Search<'a, P: Projector> {
    projector: &'a P,
    plan: &'a Plan,
    mode: Mode,
    min_balance_cents: i64,
    through_year: i32,
    used: u32,
}

/// One tried amount: whether it met the constraints, and its summary.
struct Attempt {
    cents: i64,
    passes: bool,
    summary: Summary,
}

impl Mode {
    /// The least spending financials-api accepts: a plan with drawdown years
    /// needs a withdrawal of at least 1 cent, while itemized expenses may all
    /// be $0.
    fn floor_cents(self) -> i64 {
        match self {
            Mode::Withdrawal => 1,
            Mode::Itemized { .. } => 0,
        }
    }
}

impl<P: Projector> Search<'_, P> {
    /// The plan with its spending set to `cents`.
    fn plan_at(&self, cents: i64) -> Plan {
        let mut plan = self.plan.clone();
        match self.mode {
            Mode::Withdrawal => plan.annual_withdrawal_cents = cents,
            Mode::Itemized { current_total } => {
                for expense in &mut plan.expenses {
                    expense.annual_amount_cents =
                        scale(expense.annual_amount_cents, cents, current_total);
                }
            }
        }
        plan
    }

    /// Projects the plan at `cents` and judges it. `&mut self` because it
    /// counts the projection.
    async fn attempt(&mut self, cents: i64) -> Result<Attempt, Error> {
        self.used += 1;
        let projection = self.projector.project(&self.plan_at(cents)).await?;
        Ok(Attempt {
            cents,
            passes: meets(&projection, self.min_balance_cents, self.through_year),
            summary: summarize(&projection)?,
        })
    }

    fn budget_left(&self) -> bool {
        self.used < MAX_PROJECTIONS
    }
}

/// Whether a projection meets the constraints: no unfunded spending, and the
/// balance at or above `min_balance_cents`, in every year from the first
/// retirement through `through_year`.
///
/// Saving years are skipped: a buffer like "keep $200k" is about retirement,
/// and a plan may start below it and grow past it before then.
pub fn meets(projection: &Projection, min_balance_cents: i64, through_year: i32) -> bool {
    projection
        .totals
        .iter()
        .filter(|year| year.year >= 1 && year.year <= through_year)
        // `matches!` is a `match` that returns `true` if the pattern fits.
        .filter(|year| !matches!(year.phase, Phase::Starting | Phase::Saving))
        .all(|year| year.unfunded_withdrawal_cents == 0 && year.balance_cents >= min_balance_cents)
}

/// `amount * target / total`, rounded half up, without overflow.
///
/// `amount * target` can reach about 10^18, close to `i64::MAX` (about
/// 9.2 * 10^18), so the arithmetic is done in `i128`, a 128-bit integer,
/// and converted back. The result is at most `target`, so it always fits.
fn scale(amount: i64, target: i64, total: i64) -> i64 {
    let scaled =
        (i128::from(amount) * i128::from(target) + i128::from(total) / 2) / i128::from(total);
    i64::try_from(scaled).expect("a share of the target fits in i64")
}

/// Finds the largest annual spending that meets `constraints`.
///
/// Generic over the projector, so tests use a fake with a known answer.
/// Projections run one after another: each step depends on the last result.
pub async fn find_max_spending<P: Projector>(
    projector: &P,
    plan: &Plan,
    constraints: Constraints,
) -> Result<MaxSpending, Error> {
    plan.check()?;
    let invalid = |problem: &str| Error::ConstraintShape {
        location: "constraints".to_string(),
        problem: problem.to_string(),
    };
    if !plan.uses_phase_projection() {
        return Err(invalid(
            "only works for plans with saving and drawdown phases (this plan uses `years`)",
        ));
    }
    if constraints.min_balance_cents < 0 {
        return Err(Error::ConstraintShape {
            location: "constraints.minBalanceCents".to_string(),
            problem: "must be at least 0".to_string(),
        });
    }
    let final_year = plan.last_retirement_year() + plan.drawdown_years;
    let through_year = constraints.through_year.unwrap_or(final_year);
    if !(1..=final_year).contains(&through_year) {
        return Err(Error::ConstraintShape {
            location: "constraints.throughYear".to_string(),
            problem: format!("must be between 1 and the plan's final year ({final_year})"),
        });
    }

    let (mode, current) = if plan.expenses.is_empty() {
        (Mode::Withdrawal, plan.annual_withdrawal_cents)
    } else {
        let total: i64 = plan.expenses.iter().map(|e| e.annual_amount_cents).sum();
        if total == 0 {
            return Err(invalid(
                "the itemized expenses add up to $0, so there's nothing to scale",
            ));
        }
        (
            Mode::Itemized {
                current_total: total,
            },
            total,
        )
    };

    let mut search = Search {
        projector,
        plan,
        mode,
        min_balance_cents: constraints.min_balance_cents,
        through_year,
        used: 0,
    };

    // Step 1: if it fails with the least spending allowed, nothing passes.
    let zero = search.attempt(mode.floor_cents()).await?;
    if !zero.passes {
        return Ok(result(&search, Outcome::FailsAtZero, zero, None));
    }

    // The search keeps two amounts with one rule, its *invariant*:
    //   `best` always passes, and `failing` (once found) always fails.
    // Every step moves one of them toward the other, so the answer is always
    // between them, and the gap only shrinks.
    let mut best = zero;
    let mut failing: Option<i64> = None;

    // Step 2: double from the current spending until something fails.
    let mut next = if current > 0 {
        current.min(MAX_SPENDING_CENTS)
    } else {
        DEFAULT_START_CENTS
    };
    while failing.is_none() && search.budget_left() {
        let attempt = search.attempt(next).await?;
        if attempt.passes {
            best = attempt;
            if next == MAX_SPENDING_CENTS {
                break;
            }
            // `saturating_mul` stops at `i64::MAX` instead of overflowing;
            // `min` then caps it at the search's largest amount.
            next = next.saturating_mul(2).min(MAX_SPENDING_CENTS);
        } else {
            failing = Some(next);
        }
    }

    // `let ... else`: keep going only if a failing amount was found.
    let Some(mut failing_cents) = failing else {
        return Ok(result(&search, Outcome::NoUpperLimit, best, None));
    };

    // Step 3: binary search between `best` and `failing_cents`.
    while failing_cents - best.cents > PRECISION_CENTS && search.budget_left() {
        // `best + (failing - best) / 2` rather than `(best + failing) / 2`:
        // the same midpoint, but the sum can't overflow. Here the numbers are
        // far below `i64::MAX`, but it's the habit that matters.
        let middle = best.cents + (failing_cents - best.cents) / 2;
        let attempt = search.attempt(middle).await?;
        if attempt.passes {
            best = attempt;
        } else {
            failing_cents = middle;
        }
    }

    let precision = failing_cents - best.cents;
    Ok(result(&search, Outcome::Found, best, Some(precision)))
}

/// Builds the result from the search state and the attempt to report.
fn result<P: Projector>(
    search: &Search<'_, P>,
    outcome: Outcome,
    attempt: Attempt,
    precision_cents: Option<i64>,
) -> MaxSpending {
    let found = outcome != Outcome::FailsAtZero;
    let (scale_basis_points, expenses) = match search.mode {
        Mode::Itemized { current_total } if found => (
            Some(scale(10_000, attempt.cents, current_total)),
            search
                .plan_at(attempt.cents)
                .expenses
                .into_iter()
                .map(|expense| ScaledExpense {
                    id: expense.id,
                    name: expense.name,
                    annual_amount_cents: expense.annual_amount_cents,
                })
                .collect(),
        ),
        _ => (None, Vec::new()),
    };
    MaxSpending {
        outcome,
        // `bool::then` turns `true` into `Some(value)` and `false` into `None`.
        annual_spending_cents: found.then_some(attempt.cents),
        annual_spending: found.then(|| dollars(attempt.cents)),
        scale_basis_points,
        expenses,
        precision: precision_cents.map(dollars),
        projections_used: search.used,
        min_balance: dollars(search.min_balance_cents),
        through_year: search.through_year,
        summary: attempt.summary,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::projector::FakeProjector;

    const BASIC: &str = include_str!("../tests/fixtures/plan-basic.json");
    const HOUSEHOLD: &str = include_str!("../tests/fixtures/plan-household.json");
    const PROJECTION_BASIC: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn basic() -> Plan {
        Plan::from_json_checked(BASIC).unwrap()
    }

    fn household() -> Plan {
        Plan::from_json_checked(HOUSEHOLD).unwrap()
    }

    /// A projection that passes when `spending <= limit`: below the limit,
    /// every year is funded and every balance is `limit - spending` cents
    /// plus $1,000; above it, the fixture's shortfalls remain.
    fn known_answer(spending: i64, limit: i64) -> Projection {
        let mut projection: Projection = serde_json::from_str(PROJECTION_BASIC).unwrap();
        if spending <= limit {
            for year in &mut projection.totals {
                year.unfunded_withdrawal_cents = 0;
                year.balance_cents = limit - spending + 100_000;
            }
        }
        projection
    }

    /// A fake with a known maximum withdrawal (in cents) that counts calls.
    fn withdrawal_fake(limit: i64) -> (FakeProjector, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let fake = FakeProjector::from_fn(move |plan| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(known_answer(plan.annual_withdrawal_cents, limit))
        });
        (fake, calls)
    }

    #[tokio::test]
    async fn converges_to_within_100_dollars_of_the_known_maximum() {
        let limit = 7_345_678; // $73,456.78
        let (fake, calls) = withdrawal_fake(limit);

        let found = find_max_spending(&fake, &basic(), Constraints::default())
            .await
            .unwrap();

        assert_eq!(found.outcome, Outcome::Found);
        let cents = found.annual_spending_cents.unwrap();
        assert!(cents <= limit, "the answer must pass: {cents}");
        assert!(limit - cents <= PRECISION_CENTS, "within $100: {cents}");
        assert!(found.precision.unwrap() <= 100);
        assert_eq!(found.annual_spending, Some(dollars(cents)));
        assert_eq!(found.projections_used, calls.load(Ordering::SeqCst));
        assert!(found.projections_used <= MAX_PROJECTIONS);
        // The summary is the projection at the amount found.
        assert!(found.summary.lasts_through_final_year);
        assert_eq!(
            found.summary.ending_balance,
            dollars(limit - cents + 100_000)
        );
    }

    #[tokio::test]
    async fn respects_the_projection_cap() {
        // A $10M limit from a 1-cent start: doubling up to $10M alone would
        // take 30 projections, more than the budget allows.
        let (fake, calls) = withdrawal_fake(MAX_SPENDING_CENTS - 1);
        let mut plan = basic();
        plan.annual_withdrawal_cents = 1;

        let found = find_max_spending(&fake, &plan, Constraints::default())
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), MAX_PROJECTIONS);
        assert_eq!(found.projections_used, MAX_PROJECTIONS);
        // No failing amount was reached, and what it reports still passes.
        assert_eq!(found.outcome, Outcome::NoUpperLimit);
        assert!(found.annual_spending_cents.unwrap() < MAX_SPENDING_CENTS);
    }

    #[tokio::test]
    async fn a_plan_that_fails_at_zero() {
        let (fake, calls) = withdrawal_fake(-1);

        let found = find_max_spending(&fake, &basic(), Constraints::default())
            .await
            .unwrap();

        assert_eq!(found.outcome, Outcome::FailsAtZero);
        assert_eq!(found.annual_spending_cents, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // financials-api rejects a $0 withdrawal with drawdown years, so the
        // least a withdrawal plan can try is 1 cent.
        assert_eq!(fake.received()[0].annual_withdrawal_cents, 1);
        assert!(!found.summary.lasts_through_final_year);
        let json = serde_json::to_value(&found).unwrap();
        assert_eq!(json["outcome"], "fails_at_zero");
        assert!(json.get("annualSpending").is_none(), "{json}");
    }

    #[tokio::test]
    async fn a_plan_that_passes_at_the_upper_bound() {
        let (fake, _) = withdrawal_fake(2 * MAX_SPENDING_CENTS);

        let found = find_max_spending(&fake, &basic(), Constraints::default())
            .await
            .unwrap();

        assert_eq!(found.outcome, Outcome::NoUpperLimit);
        assert_eq!(found.annual_spending_cents, Some(MAX_SPENDING_CENTS));
        assert_eq!(found.precision, None);
    }

    #[tokio::test]
    async fn works_for_itemized_expenses() {
        // Household expenses total $77,000; it passes up to $61,600 (80%).
        let limit = 6_160_000;
        let fake = FakeProjector::from_fn(move |plan| {
            let total: i64 = plan.expenses.iter().map(|e| e.annual_amount_cents).sum();
            Ok(known_answer(total, limit))
        });

        let found = find_max_spending(&fake, &household(), Constraints::default())
            .await
            .unwrap();

        assert_eq!(found.outcome, Outcome::Found);
        let cents = found.annual_spending_cents.unwrap();
        assert!(
            limit - cents <= PRECISION_CENTS && cents <= limit + 2,
            "{cents}"
        );
        // About 80% of the current expenses, each scaled by the same factor.
        let scale = found.scale_basis_points.unwrap();
        assert!((7_985..=8_000).contains(&scale), "{scale}");
        let ids: Vec<&str> = found.expenses.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["expense-living", "expense-health", "expense-travel"]);
        let living = found.expenses[0].annual_amount_cents;
        assert_eq!(living, scale_cents_like(5_500_000, cents, 7_700_000));
    }

    /// The expected scaled amount, computed independently of `scale`.
    fn scale_cents_like(amount: i64, target: i64, total: i64) -> i64 {
        ((amount as i128 * target as i128 * 2 + total as i128) / (2 * total as i128)) as i64
    }

    #[tokio::test]
    async fn min_balance_and_through_year_constrain_the_answer() {
        // Balances are `limit - spending + $1,000`, so requiring $2,000 more
        // lowers the answer by about $1,000.
        let limit = 6_000_000;
        let (fake, _) = withdrawal_fake(limit);
        let constraints = Constraints {
            min_balance_cents: 200_000,
            through_year: Some(35),
        };

        let found = find_max_spending(&fake, &basic(), constraints)
            .await
            .unwrap();

        let cents = found.annual_spending_cents.unwrap();
        assert!(cents <= limit - 100_000, "{cents}");
        assert!(limit - 100_000 - cents <= PRECISION_CENTS, "{cents}");
        assert_eq!(found.min_balance, 2_000);
        assert_eq!(found.through_year, 35);
    }

    #[test]
    fn meets_skips_saving_years_and_years_after_through_year() {
        let mut projection: Projection = serde_json::from_str(PROJECTION_BASIC).unwrap();
        // The fixture runs short from year 36 of 40.
        assert!(!meets(&projection, 0, 40));
        assert!(meets(&projection, 0, 35));
        // Year 1 is a saving year: a balance below the minimum there doesn't
        // count.
        assert_eq!(projection.totals[1].phase, Phase::Saving);
        projection.totals[1].balance_cents = 0;
        assert!(meets(&projection, 1, 35));
        // Year 20 is a drawdown year: there it does.
        assert_eq!(projection.totals[20].phase, Phase::Drawdown);
        projection.totals[20].balance_cents = 0;
        assert!(!meets(&projection, 1, 35));
        assert!(meets(&projection, 0, 35));
    }

    #[tokio::test]
    async fn constraints_are_checked() {
        let (fake, calls) = withdrawal_fake(1);
        let error = |constraints| {
            let plan = basic();
            let fake = &fake;
            async move {
                find_max_spending(fake, &plan, constraints)
                    .await
                    .unwrap_err()
                    .to_string()
            }
        };

        assert_eq!(
            error(Constraints {
                min_balance_cents: -1,
                through_year: None
            })
            .await,
            "constraint problem at `constraints.minBalanceCents`: must be at least 0"
        );
        assert_eq!(
            error(Constraints {
                min_balance_cents: 0,
                through_year: Some(41)
            })
            .await,
            "constraint problem at `constraints.throughYear`: must be between 1 and the plan's final year (40)"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn simple_year_plans_are_rejected() {
        let (fake, _) = withdrawal_fake(1);
        let mut plan = basic();
        plan.saving_years = 0;
        plan.drawdown_years = 0;
        plan.annual_withdrawal_cents = 0;
        plan.annual_withdrawal_inflation_rate_basis_points = 0;
        plan.years = 30;

        let err = find_max_spending(&fake, &plan, Constraints::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("this plan uses `years`"), "{err}");
    }

    #[test]
    fn constraints_parse_with_value_free_errors() {
        assert_eq!(
            Constraints::from_value(serde_json::Value::Null).unwrap(),
            Constraints::default()
        );
        let parsed =
            Constraints::from_value(serde_json::json!({"minBalanceCents": 20000000})).unwrap();
        assert_eq!(parsed.min_balance_cents, 20_000_000);
        assert_eq!(parsed.through_year, None);

        let err = Constraints::from_value(serde_json::json!({"minBalanceCents": 1.5}))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "constraint problem at `constraints.minBalanceCents`: invalid type, expected a whole number (cents or basis points, no decimals)"
        );
        let err = Constraints::from_value(serde_json::json!({"minBalance": 1}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field `minBalance`"), "{err}");
    }

    #[test]
    fn scaling_uses_wide_integers() {
        assert_eq!(scale(5_500_000, 6_160_000, 7_700_000), 4_400_000);
        // Big enough to overflow `i64` if multiplied directly.
        assert_eq!(
            scale(MAX_SPENDING_CENTS, MAX_SPENDING_CENTS, MAX_SPENDING_CENTS),
            MAX_SPENDING_CENTS
        );
        assert_eq!(scale(10_000, 1, 3), 3_333);
    }
}
