//! **Comparing scenarios**: one baseline plus several "what ifs", projected
//! concurrently.
//!
//! A scenario is a name and a list of [`Override`]s. [`compare_scenarios`]
//! projects the baseline plan and every scenario, at most
//! [`MAX_CONCURRENT`] at a time, and returns one row per scenario with its
//! summary and its change from the baseline. A scenario that fails (a bad
//! override, an API error) becomes an error row; the others still run.
//!
//! # Concurrency, not parallelism
//!
//! The server runs on tokio's single-threaded runtime (`current_thread` in
//! `main.rs`), and that's enough: a projection spends nearly all its time
//! waiting for financials-api to answer. While one task waits, the runtime
//! runs another, so four requests are in flight at once on one thread. Go
//! would start goroutines; Python would `asyncio.gather` coroutines. No
//! extra threads (`rt-multi-thread`) are needed for work that's mostly
//! waiting.

use std::collections::HashSet;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::error::Error;
use crate::overrides::{Override, apply_overrides};
use crate::plan::Plan;
use crate::projector::{Projector, project_and_summarize};
use crate::summary::Summary;

/// Most scenarios per call (plus the baseline, 13 projections).
pub const MAX_SCENARIOS: usize = 12;

/// Most projections in flight at once, to be polite to financials-api.
pub const MAX_CONCURRENT: usize = 4;

/// Longest scenario name accepted, in characters.
const MAX_NAME_CHARS: usize = 80;

/// One named "what if": overrides applied to a copy of the baseline plan.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scenario {
    /// A short label, unique within the call, such as "Retire 2 years earlier".
    pub name: String,
    /// Changes to the baseline plan, applied in order. Empty means the
    /// baseline itself.
    #[serde(default)]
    pub overrides: Vec<Override>,
}

impl Scenario {
    /// Parses and checks a list of scenarios from a tool call's JSON.
    ///
    /// Problems name *where* they are (`scenarios[1].overrides[0].op`) but
    /// never quote a value.
    pub fn list_from_value(value: serde_json::Value) -> Result<Vec<Scenario>, Error> {
        let scenarios: Vec<Scenario> = serde_path_to_error::deserialize(value).map_err(|err| {
            let path = err.path().to_string();
            match Error::from_json_error(&path, err.inner()) {
                Error::PlanShape { problem, .. } => Error::ScenarioShape {
                    location: format!("scenarios{}", path.trim_start_matches('.')),
                    problem,
                },
                other => other,
            }
        })?;
        check_scenarios(&scenarios)?;
        Ok(scenarios)
    }
}

/// Count, name and uniqueness rules for a list of scenarios.
fn check_scenarios(scenarios: &[Scenario]) -> Result<(), Error> {
    let shape = |location: String, problem: String| Error::ScenarioShape { location, problem };

    if scenarios.is_empty() || scenarios.len() > MAX_SCENARIOS {
        return Err(shape(
            "scenarios".to_string(),
            format!("give between 1 and {MAX_SCENARIOS} scenarios"),
        ));
    }
    // A `HashSet` holds unique values, like a Go `map[string]struct{}` or a
    // Python `set`. `insert` returns `false` if the value was already there.
    // It stores `&str`s borrowed from `scenarios`, so nothing is copied.
    let mut seen: HashSet<&str> = HashSet::new();
    for (index, scenario) in scenarios.iter().enumerate() {
        let name = scenario.name.trim();
        if name.is_empty() {
            return Err(shape(
                format!("scenarios[{index}].name"),
                "a name is required".to_string(),
            ));
        }
        // `.chars().count()` counts characters; `.len()` would count bytes,
        // which differ for anything beyond ASCII (Rust strings are UTF-8).
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(shape(
                format!("scenarios[{index}].name"),
                format!("names can be at most {MAX_NAME_CHARS} characters"),
            ));
        }
        if !seen.insert(name) {
            return Err(shape(
                format!("scenarios[{index}].name"),
                "scenario names must be unique".to_string(),
            ));
        }
    }
    Ok(())
}

/// The baseline summary and one row per scenario, in request order.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub baseline: Summary,
    pub scenarios: Vec<ScenarioRow>,
}

/// One scenario's outcome.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioRow {
    pub name: String,
    // `flatten` writes the outcome's fields directly into the row, so a row
    // is `{"name": ..., "summary": ..., "changeFromBaseline": ...}` or
    // `{"name": ..., "error": ...}`, with no extra nesting.
    #[serde(flatten)]
    pub outcome: Outcome,
}

// `untagged` serializes just the variant's fields, with no variant name.
// That's fine for output: the fields themselves (`summary` versus `error`)
// show which case it is.
/// A projected scenario, or why it couldn't be projected.
#[derive(Debug, Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum Outcome {
    Projected {
        summary: Summary,
        change_from_baseline: Change,
    },
    Failed {
        error: String,
    },
}

/// A scenario's numbers minus the baseline's: positive means "more" (a later
/// depletion year, more years short, a bigger balance). Whole dollars.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Years later (+) or earlier (-) the money runs short. Only present when
    /// both run short; otherwise compare `lastsThroughFinalYear`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depletion_year: Option<i32>,
    pub years_short: i64,
    pub ending_balance: i64,
    pub lowest_balance: i64,
    pub total_unfunded: i64,
}

impl Change {
    /// `scenario - baseline` for each compared number.
    pub fn between(baseline: &Summary, scenario: &Summary) -> Change {
        Change {
            // Matching a *tuple* of two `Option`s handles all four cases at
            // once; only "both `Some`" gives a difference.
            depletion_year: match (baseline.depletion_year, scenario.depletion_year) {
                (Some(base), Some(other)) => Some(other - base),
                _ => None,
            },
            // `years_short` is a `usize` (a count can't be negative), but a
            // difference can be. `as i64` converts; a count of years always
            // fits, so nothing is lost.
            years_short: scenario.years_short as i64 - baseline.years_short as i64,
            ending_balance: scenario.ending_balance - baseline.ending_balance,
            lowest_balance: scenario.lowest_balance.amount - baseline.lowest_balance.amount,
            total_unfunded: scenario.total_unfunded - baseline.total_unfunded,
        }
    }
}

/// Projects `plan` and every scenario, at most [`MAX_CONCURRENT`] at once.
///
/// The baseline must succeed, since every row is compared with it: a bad base
/// plan or a failed baseline projection fails the whole call. A failing
/// scenario only fails its own row.
///
/// `projector: Arc<P>` is *shared ownership*: each spawned task gets its own
/// `Arc` clone (a pointer copy plus a reference count), and the projector is
/// dropped when the last one is gone. Spawned tasks can't borrow from this
/// function, because they might outlive it; owning an `Arc` solves that.
pub async fn compare_scenarios<P: Projector + 'static>(
    projector: Arc<P>,
    plan: &Plan,
    scenarios: &[Scenario],
) -> Result<Comparison, Error> {
    plan.check()?;
    check_scenarios(scenarios)?;

    // Prepare every run's plan up front. Index 0 is the baseline; a scenario
    // whose overrides don't apply keeps its error and is never projected.
    // `std::iter::once(..).chain(..)` builds "the baseline, then each
    // scenario" as one sequence.
    let prepared: Vec<Result<Plan, Error>> = std::iter::once(Ok(plan.clone()))
        .chain(
            scenarios
                .iter()
                .map(|scenario| apply_overrides(plan, &scenario.overrides)),
        )
        .collect();

    // One slot per run, filled in as results arrive in whatever order.
    let mut results: Vec<Option<Result<Summary, Error>>> = prepared.iter().map(|_| None).collect();

    // A semaphore holds `MAX_CONCURRENT` permits. Each task waits for a
    // permit before projecting and gives it back when the permit is dropped,
    // so at most four projections run at once. Go would use a buffered
    // channel as a semaphore; Python, `asyncio.Semaphore`.
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT));
    // A `JoinSet` owns a group of spawned tasks and hands back their results
    // as they finish, like a Go `errgroup` or Python's `asyncio.as_completed`.
    let mut tasks = JoinSet::new();

    for (index, run) in prepared.into_iter().enumerate() {
        match run {
            Err(err) => results[index] = Some(Err(err)),
            Ok(run_plan) => {
                let projector = Arc::clone(&projector);
                let permits = Arc::clone(&permits);
                // `async move` makes a future that *owns* everything it uses
                // (`projector`, `permits`, `run_plan`, `index`). `spawn`
                // requires that (`'static`): the task may run after this loop
                // iteration, or even this function, has moved on.
                tasks.spawn(async move {
                    // Held until the end of the block, then dropped, which
                    // returns the permit. The semaphore is never closed, so
                    // `acquire_owned` can't fail.
                    let _permit = permits
                        .acquire_owned()
                        .await
                        .expect("the semaphore is never closed");
                    (index, project_and_summarize(&*projector, &run_plan).await)
                });
            }
        }
    }

    // `join_next` waits for the next task to finish, in completion order.
    // Each result carries its index, so it goes back in its own slot and the
    // rows come out in request order. An `Err` here means the task itself
    // panicked; its slot stays empty and is reported below.
    while let Some(joined) = tasks.join_next().await {
        if let Ok((index, result)) = joined {
            results[index] = Some(result);
        }
    }

    let mut results = results.into_iter().map(|slot| {
        slot.unwrap_or_else(|| {
            Err(Error::Server {
                problem: "a projection task stopped unexpectedly".to_string(),
            })
        })
    });

    // The first result is the baseline; `?` fails the whole call if it did.
    let baseline = results.next().expect("the baseline is always run")?;
    // `zip` pairs each scenario with its result, in order.
    let rows = scenarios
        .iter()
        .zip(results)
        .map(|(scenario, result)| ScenarioRow {
            name: scenario.name.trim().to_string(),
            outcome: match result {
                Ok(summary) => Outcome::Projected {
                    change_from_baseline: Change::between(&baseline, &summary),
                    summary,
                },
                Err(err) => Outcome::Failed {
                    error: err.to_string(),
                },
            },
        })
        .collect();

    Ok(Comparison {
        baseline,
        scenarios: rows,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    const BASIC: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION_BASIC: &str = include_str!("../tests/fixtures/projection-basic.json");
    const PROJECTION_HOUSEHOLD: &str = include_str!("../tests/fixtures/projection-household.json");

    fn plan() -> Plan {
        Plan::from_json_checked(BASIC).unwrap()
    }

    fn projection(json: &str) -> Projection {
        serde_json::from_str(json).unwrap()
    }

    fn scenarios(json: &str) -> Vec<Scenario> {
        Scenario::list_from_value(serde_json::from_str(json).unwrap()).unwrap()
    }

    fn scenario_error(json: &str) -> String {
        Scenario::list_from_value(serde_json::from_str(json).unwrap())
            .unwrap_err()
            .to_string()
    }

    /// The `Projected` parts of a row, or a panic naming the row's error.
    fn projected(row: &ScenarioRow) -> (&Summary, &Change) {
        match &row.outcome {
            Outcome::Projected {
                summary,
                change_from_baseline,
            } => (summary, change_from_baseline),
            Outcome::Failed { error } => panic!("{} failed: {error}", row.name),
        }
    }

    fn failed(row: &ScenarioRow) -> &str {
        match &row.outcome {
            Outcome::Failed { error } => error,
            Outcome::Projected { .. } => panic!("{} should have failed", row.name),
        }
    }

    /// A fake that answers with the household projection (runs short in year
    /// 24) for 8 saving years, and the basic one (year 36) otherwise.
    fn two_outcome_fake() -> FakeProjector {
        FakeProjector::from_fn(|plan| {
            Ok(if plan.saving_years == 8 {
                projection(PROJECTION_HOUSEHOLD)
            } else {
                projection(PROJECTION_BASIC)
            })
        })
    }

    #[tokio::test]
    async fn rows_come_back_in_request_order_with_differences() {
        let list = scenarios(
            r#"[
                {"name": "Retire 2 years earlier", "overrides": [{"op": "shift_retirement", "years": -2}]},
                {"name": "Spend 15% less", "overrides": [{"op": "scale_expenses", "percent": 85}]}
            ]"#,
        );

        let comparison = compare_scenarios(Arc::new(two_outcome_fake()), &plan(), &list)
            .await
            .unwrap();

        assert_eq!(comparison.baseline.depletion_year, Some(36));
        let names: Vec<&str> = comparison
            .scenarios
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, ["Retire 2 years earlier", "Spend 15% less"]);

        let (summary, change) = projected(&comparison.scenarios[0]);
        assert_eq!(summary.depletion_year, Some(24));
        assert_eq!(change.depletion_year, Some(-12));
        assert_eq!(change.years_short, 13);
        assert_eq!(
            change.total_unfunded,
            summary.total_unfunded - comparison.baseline.total_unfunded
        );

        // Same projection as the baseline: no change at all.
        let (_, change) = projected(&comparison.scenarios[1]);
        assert_eq!(
            *change,
            Change {
                depletion_year: Some(0),
                years_short: 0,
                ending_balance: 0,
                lowest_balance: 0,
                total_unfunded: 0,
            }
        );
    }

    #[test]
    fn depletion_change_is_absent_unless_both_run_short() {
        let short = crate::summary::summarize(&projection(PROJECTION_BASIC)).unwrap();
        let mut lasts = short.clone();
        lasts.depletion_year = None;
        lasts.years_short = 0;

        let change = Change::between(&short, &lasts);
        assert_eq!(change.depletion_year, None);
        assert_eq!(change.years_short, -5);
        let json = serde_json::to_value(change).unwrap();
        assert!(json.get("depletionYear").is_none(), "{json}");
    }

    /// A projector that counts how many projections are running at once.
    ///
    /// `AtomicUsize` is an integer that many tasks can update safely without a
    /// lock (like Go's `atomic.Int64`). Each projection yields to the runtime
    /// several times, so other tasks get to start while it's "in flight";
    /// plans with fewer drawdown years yield more, so later scenarios finish
    /// first. Its ending balance is the plan's drawdown years, in dollars, so
    /// a test can tell which plan a row came from.
    #[derive(Default)]
    struct CountingProjector {
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        calls: AtomicUsize,
    }

    impl Projector for CountingProjector {
        async fn project(&self, plan: &Plan) -> Result<Projection, Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);

            for _ in 0..(60 - plan.drawdown_years) {
                tokio::task::yield_now().await;
            }

            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let mut result = projection(PROJECTION_BASIC);
            let last = result.totals.last_mut().unwrap();
            last.balance_cents = i64::from(plan.drawdown_years) * 100;
            Ok(result)
        }
    }

    /// `count` scenarios, each setting drawdown to 21, 22, 23, ...
    fn drawdown_scenarios(count: usize) -> Vec<Scenario> {
        (0..count)
            .map(|n| Scenario {
                name: format!("Drawdown {}", 21 + n),
                overrides: vec![Override::SetDrawdownYears {
                    years: 21 + n as i32,
                }],
            })
            .collect()
    }

    #[tokio::test]
    async fn no_more_than_four_projections_run_at_once() {
        let projector = Arc::new(CountingProjector::default());

        let comparison = compare_scenarios(
            Arc::clone(&projector),
            &plan(),
            &drawdown_scenarios(MAX_SCENARIOS),
        )
        .await
        .unwrap();

        assert_eq!(projector.calls.load(Ordering::SeqCst), 13);
        // Exactly 4: the limit holds, *and* they really did run concurrently.
        assert_eq!(projector.max_in_flight.load(Ordering::SeqCst), 4);
        assert_eq!(comparison.scenarios.len(), 12);
    }

    #[tokio::test]
    async fn rows_stay_in_request_order_when_tasks_finish_out_of_order() {
        let projector = Arc::new(CountingProjector::default());

        let comparison = compare_scenarios(projector, &plan(), &drawdown_scenarios(8))
            .await
            .unwrap();

        // Each row's ending balance is its own plan's drawdown years, even
        // though higher drawdowns finished first.
        for (n, row) in comparison.scenarios.iter().enumerate() {
            let (summary, _) = projected(row);
            assert_eq!(row.name, format!("Drawdown {}", 21 + n));
            assert_eq!(summary.ending_balance, 21 + n as i64);
        }
        assert_eq!(comparison.baseline.ending_balance, 30);
    }

    #[tokio::test]
    async fn one_failed_scenario_does_not_fail_the_others() {
        let fake = FakeProjector::from_fn(|plan| {
            if plan.drawdown_years == 25 {
                Err(Error::Api {
                    problem: "timed out".to_string(),
                })
            } else {
                Ok(projection(PROJECTION_BASIC))
            }
        });
        let list = scenarios(
            r#"[
                {"name": "Shorter drawdown", "overrides": [{"op": "set_drawdown_years", "years": 25}]},
                {"name": "Bad override", "overrides": [{"op": "remove_expense", "expenseId": "expense-boat"}]},
                {"name": "Lower returns", "overrides": [{"op": "adjust_returns", "basisPoints": -100}]}
            ]"#,
        );
        let fake = Arc::new(fake);

        let comparison = compare_scenarios(Arc::clone(&fake), &plan(), &list)
            .await
            .unwrap();

        assert_eq!(
            failed(&comparison.scenarios[0]),
            "financials-api is unavailable: timed out"
        );
        assert_eq!(
            failed(&comparison.scenarios[1]),
            "overrides[0] (remove_expense): no expense with id `expense-boat`"
        );
        projected(&comparison.scenarios[2]);
        // The bad override was never sent: baseline + two scenarios.
        assert_eq!(fake.received().len(), 3);

        let json = serde_json::to_value(&comparison).unwrap();
        assert_eq!(
            json["scenarios"][1],
            serde_json::json!({
                "name": "Bad override",
                "error": "overrides[0] (remove_expense): no expense with id `expense-boat`"
            })
        );
        assert!(
            json["scenarios"][2]["changeFromBaseline"].is_object(),
            "{json}"
        );
    }

    #[tokio::test]
    async fn a_failed_baseline_fails_the_call() {
        let fake = FakeProjector::scripted(vec![Err(Error::Auth)]);
        let result = compare_scenarios(Arc::new(fake), &plan(), &drawdown_scenarios(1)).await;
        assert!(matches!(result, Err(Error::Auth)), "{result:?}");
    }

    #[tokio::test]
    async fn an_invalid_base_plan_is_rejected_before_projecting() {
        let mut bad = plan();
        bad.drawdown_years = 99;
        let fake = Arc::new(FakeProjector::scripted(Vec::new()));

        let err = compare_scenarios(Arc::clone(&fake), &bad, &drawdown_scenarios(1))
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "invalid plan: drawdownYears must be between 0 and 75"
        );
        assert!(fake.received().is_empty());
    }

    #[test]
    fn scenario_count_and_names_are_checked() {
        let named = |names: &[&str]| {
            serde_json::to_string(
                &names
                    .iter()
                    .map(|name| serde_json::json!({ "name": name }))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };

        assert_eq!(
            scenario_error("[]"),
            "scenario problem at `scenarios`: give between 1 and 12 scenarios"
        );
        let thirteen: Vec<String> = (0..13).map(|n| format!("S{n}")).collect();
        let thirteen: Vec<&str> = thirteen.iter().map(String::as_str).collect();
        assert_eq!(
            scenario_error(&named(&thirteen)),
            "scenario problem at `scenarios`: give between 1 and 12 scenarios"
        );
        assert_eq!(
            scenario_error(&named(&["A", "  "])),
            "scenario problem at `scenarios[1].name`: a name is required"
        );
        assert_eq!(
            scenario_error(&named(&["Same", "Same "])),
            "scenario problem at `scenarios[1].name`: scenario names must be unique"
        );
        assert_eq!(
            scenario_error(&named(&[&"x".repeat(81)])),
            "scenario problem at `scenarios[0].name`: names can be at most 80 characters"
        );
        // No overrides is allowed: that row is the baseline again.
        assert_eq!(scenarios(&named(&["As is"]))[0].overrides, []);
    }

    #[test]
    fn scenario_shape_errors_point_into_the_list() {
        assert_eq!(
            scenario_error(r#"[{"name": "A", "overrides": [{"op": "retire_now"}]}]"#)
                .split(',')
                .next()
                .unwrap(),
            "scenario problem at `scenarios[0].overrides[0].op`: unknown variant `retire_now`"
        );
        assert_eq!(
            scenario_error(r#"[{"overrides": []}]"#),
            "scenario problem at `scenarios[0]`: missing field `name`"
        );
    }
}
