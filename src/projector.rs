//! Running a projection, behind an interface.
//!
//! Everything built on top of projections (comparisons, sweeps, searches, the
//! MCP tools) talks to a [`Projector`], never to HTTP directly. In production
//! that's the financials-api client (Step 07); in tests it's
//! [`FakeProjector`], so no test ever needs a network or a running API.
//!
//! # Async in one paragraph
//!
//! An `async fn` doesn't run when you call it. It returns a **future**: a
//! value describing work that hasn't happened yet. `.await` runs the future
//! until it finishes, letting other work proceed while it waits (for a network
//! response, say). Futures need a **runtime** to drive them; this project uses
//! `tokio`, the standard one. It's like Python's `asyncio` (`async def` /
//! `await`). Go hides all of this behind goroutines, which the Go runtime
//! schedules for you.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;

use crate::error::Error;
use crate::plan::Plan;
use crate::projection::Projection;
use crate::summary::{Summary, summarize};

/// Something that can run a projection for a plan.
///
/// A trait is like a Go interface: a set of methods a type promises to have.
/// Unlike Go, a type implements it explicitly with `impl Projector for ...`.
///
/// # Why `-> impl Future<...> + Send` instead of `async fn`
///
/// `async fn project(&self, plan: &Plan) -> Result<Projection, Error>` would
/// mean the same thing: an async method is just a method returning
/// `impl Future`. Writing it out lets the trait add `+ Send`, a promise that
/// the future can be moved between threads. Later steps run several
/// projections in parallel on tokio's multi-threaded runtime, which requires
/// that promise. Implementations can still be written with plain `async fn`.
///
/// The bound matters in **generic** code (`P: Projector`), where the compiler
/// knows nothing about a projector beyond what this trait promises; the test
/// `a_projector_can_be_shared_across_tasks` shows it. Writing plain `async fn`
/// in a public trait also triggers a compiler warning for exactly this reason.
///
/// `Send + Sync` on the trait itself says a projector can be shared between
/// threads (for example, behind an `Arc`).
pub trait Projector: Send + Sync {
    /// Runs one projection. The plan is borrowed: the projector reads it but
    /// the caller keeps it.
    fn project(&self, plan: &Plan) -> impl Future<Output = Result<Projection, Error>> + Send;
}

/// Pre-checks a plan, runs it, and summarizes the result: the core of every
/// tool to come.
///
/// This is a **generic function**. `<P: Projector>` means "for any type `P`
/// that implements `Projector`". The compiler generates a version of the
/// function for each type it's used with, so there's no runtime cost for the
/// abstraction. Go generics with an interface constraint work much the same.
pub async fn project_and_summarize<P: Projector>(
    projector: &P,
    plan: &Plan,
) -> Result<Summary, Error> {
    plan.check()?;
    // `.await` waits for the projection; `?` then returns early on an error.
    let projection = projector.project(plan).await?;
    summarize(&projection)
}

/// A function that computes a projection for a plan, used by
/// [`FakeProjector::from_fn`].
///
/// `Box<dyn Fn(&Plan) -> ... + Send + Sync>` is a **trait object**: "some
/// function with this signature, decided at runtime". `dyn` means calls go
/// through a pointer to the right code (dynamic dispatch, like calling a Go
/// interface method), and `Box` puts it on the heap because different
/// closures have different sizes. Compare `<P: Projector>` above, which is
/// resolved at compile time.
type ProjectionFn = Box<dyn Fn(&Plan) -> Result<Projection, Error> + Send + Sync>;

/// A test double for [`Projector`]: no network, fully predictable.
///
/// Two modes:
/// - **scripted:** returns queued responses in order ([`FakeProjector::scripted`]);
/// - **computed:** calls a function you supply for each plan
///   ([`FakeProjector::from_fn`]), handy when a test runs many projections.
///
/// Either way it records every plan it receives, so tests can check what was
/// asked.
///
/// It's part of the library (not test-only code) so the integration tests in
/// `tests/` can use it too. It never touches the network or real data.
pub struct FakeProjector {
    responses: Responses,
    // `Mutex` makes the list safe to change from several threads at once:
    // only one can hold the lock at a time. `project` takes `&self` (a
    // shared borrow), so without the `Mutex` the compiler wouldn't let it
    // modify anything. This is called *interior mutability*.
    received: Mutex<Vec<Plan>>,
}

/// The two ways a fake can answer. An enum with data, like `Error`.
enum Responses {
    Scripted(Mutex<VecDeque<Result<Projection, Error>>>),
    Computed(ProjectionFn),
}

impl FakeProjector {
    /// Answers with these results, in order, one per call.
    pub fn scripted(responses: Vec<Result<Projection, Error>>) -> FakeProjector {
        FakeProjector {
            responses: Responses::Scripted(Mutex::new(responses.into())),
            received: Mutex::new(Vec::new()),
        }
    }

    /// Answers by calling `compute` with each plan.
    ///
    /// `impl Fn(...) + Send + Sync + 'static` accepts any closure with that
    /// shape. `'static` means it doesn't borrow anything short-lived (it may
    /// own data, via `move`), so it's safe to keep inside the fake for as long
    /// as the fake exists.
    pub fn from_fn(
        compute: impl Fn(&Plan) -> Result<Projection, Error> + Send + Sync + 'static,
    ) -> FakeProjector {
        FakeProjector {
            responses: Responses::Computed(Box::new(compute)),
            received: Mutex::new(Vec::new()),
        }
    }

    /// The plans received so far, in order (copies, so the caller owns them).
    pub fn received(&self) -> Vec<Plan> {
        self.received.lock().expect("lock poisoned").clone()
    }
}

impl Projector for FakeProjector {
    // The trait declares `-> impl Future + Send`; implementing it with an
    // ordinary `async fn` is allowed as long as the future really is `Send`.
    async fn project(&self, plan: &Plan) -> Result<Projection, Error> {
        // `.lock()` waits for the mutex and returns a guard; the lock is
        // released automatically when the guard goes out of scope, at the end
        // of this statement. That's RAII: like `defer mu.Unlock()` in Go, but
        // the compiler inserts it.
        self.received
            .lock()
            .expect("lock poisoned")
            .push(plan.clone());

        match &self.responses {
            Responses::Scripted(queue) => queue
                .lock()
                .expect("lock poisoned")
                .pop_front()
                .expect("FakeProjector ran out of scripted responses"),
            Responses::Computed(compute) => compute(plan),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::projection::{Phase, YearlyBalance};

    const PLAN: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn plan() -> Plan {
        Plan::from_json(PLAN).unwrap()
    }

    fn real_projection() -> Projection {
        serde_json::from_str(PROJECTION).unwrap()
    }

    /// A one-year projection whose ending balance is `balance_cents`.
    fn one_year(balance_cents: i64) -> Projection {
        let mut projection = real_projection();
        projection.totals = vec![YearlyBalance {
            year: 0,
            phase: Phase::Starting,
            balance_cents,
            contribution_cents: 0,
            withdrawal_cents: 0,
            growth_cents: 0,
            unfunded_withdrawal_cents: 0,
            locked_balance_cents: 0,
        }];
        projection
    }

    // `#[tokio::test]` starts a tokio runtime for the test so it can be an
    // `async fn` and use `.await`. A plain `#[test]` can't await.
    #[tokio::test]
    async fn scripted_responses_come_back_in_order() {
        let fake = FakeProjector::scripted(vec![Ok(one_year(100)), Ok(one_year(200))]);

        let first = fake.project(&plan()).await.unwrap();
        let second = fake.project(&plan()).await.unwrap();

        assert_eq!(first.totals[0].balance_cents, 100);
        assert_eq!(second.totals[0].balance_cents, 200);
        assert_eq!(fake.received().len(), 2);
    }

    #[tokio::test]
    async fn the_fake_records_the_plans_it_receives() {
        let fake = FakeProjector::scripted(vec![Ok(one_year(1))]);
        let mut plan = plan();
        plan.drawdown_years = 25;

        fake.project(&plan).await.unwrap();

        assert_eq!(fake.received(), vec![plan]);
    }

    #[tokio::test]
    async fn the_fake_can_return_an_error() {
        let fake = FakeProjector::scripted(vec![Err(Error::UnexpectedResponse {
            problem: "scripted failure".to_string(),
        })]);

        let err = fake.project(&plan()).await.unwrap_err();
        assert!(err.to_string().contains("scripted failure"), "{err}");
    }

    #[tokio::test]
    async fn computed_mode_answers_from_the_plan() {
        // A closure that "projects" by turning drawdown years into a balance:
        // nothing like the real math, just predictable.
        let fake =
            FakeProjector::from_fn(|plan| Ok(one_year(i64::from(plan.drawdown_years) * 100)));

        let mut plan = plan();
        plan.drawdown_years = 7;
        assert_eq!(
            fake.project(&plan).await.unwrap().totals[0].balance_cents,
            700
        );
    }

    #[tokio::test]
    #[should_panic(expected = "ran out of scripted responses")]
    async fn running_out_of_scripted_responses_is_a_test_bug() {
        let fake = FakeProjector::scripted(Vec::new());
        let _ = fake.project(&plan()).await;
    }

    #[tokio::test]
    async fn project_and_summarize_checks_runs_and_summarizes() {
        let fake = FakeProjector::scripted(vec![Ok(real_projection())]);

        let summary = project_and_summarize(&fake, &plan()).await.unwrap();

        assert_eq!(summary.depletion_year, Some(36));
        assert_eq!(fake.received().len(), 1);
    }

    #[tokio::test]
    async fn an_invalid_plan_never_reaches_the_projector() {
        let fake = FakeProjector::scripted(Vec::new());
        let mut plan = plan();
        plan.items.clear();

        let err = project_and_summarize(&fake, &plan).await.unwrap_err();

        assert!(matches!(err, Error::InvalidPlan { .. }), "{err}");
        assert!(
            fake.received().is_empty(),
            "check() must run before projecting"
        );
    }

    /// Runs a projection on a separate tokio task, for *any* projector.
    ///
    /// `Arc` ("atomically reference counted") lets several owners share one
    /// value, freed when the last `Arc` goes away; `Arc::clone` copies the
    /// pointer, not the projector. `tokio::spawn` may run the task on another
    /// thread, so the future must be `Send`. In generic code like this, the
    /// compiler only knows what the `Projector` trait promises about `P`, so
    /// without `+ Send` on `project`'s return type this function would not
    /// compile. (With a concrete type such as `FakeProjector`, the compiler can
    /// see the real future and works out that it's `Send` on its own.)
    async fn project_in_task<P: Projector + 'static>(
        projector: Arc<P>,
        plan: Plan,
    ) -> Result<Projection, Error> {
        tokio::spawn(async move { projector.project(&plan).await })
            .await
            .expect("task should not panic")
    }

    #[tokio::test]
    async fn a_projector_can_be_shared_across_tasks() {
        let fake = Arc::new(FakeProjector::from_fn(|_| Ok(one_year(42))));

        let projection = project_in_task(Arc::clone(&fake), plan()).await.unwrap();

        assert_eq!(projection.totals[0].balance_cents, 42);
        assert_eq!(fake.received().len(), 1, "both Arcs point at the same fake");
    }
}
