//! **Batches**: project many plans concurrently, politely.
//!
//! [`project_all`] is the engine behind `compare_scenarios` and `sweep`. It
//! takes a list of prepared plans (or the errors from preparing them),
//! projects the plans at most [`MAX_CONCURRENT`] at a time, and returns one
//! result per input, in input order.
//!
//! # Concurrency, not parallelism
//!
//! The server runs on tokio's single-threaded runtime (`current_thread` in
//! `main.rs`), and that's enough: a projection spends nearly all its time
//! waiting for financials-api to answer. While one task waits, the runtime
//! runs another, so four requests are in flight at once on one thread. Go
//! would start goroutines; Python would `asyncio.gather` coroutines.

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::Instrument;

use crate::error::Error;
use crate::plan::Plan;
use crate::projector::{Projector, project_and_summarize};
use crate::summary::Summary;

/// Most projections in flight at once, to be polite to financials-api.
pub const MAX_CONCURRENT: usize = 4;

/// Projects every `Ok` plan, at most [`MAX_CONCURRENT`] at once, and returns
/// one result per input in the same order. An `Err` input (a plan that
/// couldn't be prepared) is passed through without projecting.
///
/// This is a **generic function**: `<P: Projector + 'static>` means it works
/// for any projector type, and the compiler generates a copy specialized for
/// each one it's used with (`ApiProjector` in the binary, fakes in tests).
/// Go generics and Python type hints look similar, but Python's are never
/// checked at runtime and Go's are usually not specialized per type.
///
/// `projector: Arc<P>` is *shared ownership*: each spawned task gets its own
/// `Arc` clone (a pointer copy plus a reference count). Spawned tasks can't
/// borrow from this function, because they might outlive it.
pub async fn project_all<P: Projector + 'static>(
    projector: Arc<P>,
    prepared: Vec<Result<Plan, Error>>,
) -> Vec<Result<Summary, Error>> {
    // One slot per input, filled in as results arrive in whatever order.
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
            Ok(plan) => {
                let projector = Arc::clone(&projector);
                let permits = Arc::clone(&permits);
                // `async move` makes a future that *owns* everything it uses
                // (`projector`, `permits`, `plan`, `index`). `spawn` requires
                // that (`'static`): the task may run after this loop
                // iteration, or even this function, has moved on.
                // A spawned task starts outside any span, so the caller's
                // span (a tool call's `tool=...`) is attached explicitly;
                // `Span::current()` is whichever span is active right here.
                tasks.spawn(
                    async move {
                        // Held until the end of the block, then dropped, which
                        // returns the permit. (`let _ = ...` would drop it at once
                        // and the limit would do nothing.)
                        let _permit = permits
                            .acquire_owned()
                            .await
                            .expect("the semaphore is never closed");
                        (index, project_and_summarize(&*projector, &plan).await)
                    }
                    .instrument(tracing::Span::current()),
                );
            }
        }
    }

    // `join_next` waits for the next task to finish, in completion order.
    // Each result carries its index, so it goes back in its own slot. An
    // `Err` here means the task itself panicked; its slot stays empty.
    while let Some(joined) = tasks.join_next().await {
        if let Ok((index, result)) = joined {
            results[index] = Some(result);
        }
    }

    results
        .into_iter()
        .map(|slot| {
            slot.unwrap_or_else(|| {
                Err(Error::Server {
                    problem: "a projection task stopped unexpectedly".to_string(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::projection::Projection;

    const BASIC: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION_BASIC: &str = include_str!("../tests/fixtures/projection-basic.json");

    /// A projector that counts how many projections are running at once.
    ///
    /// `AtomicUsize` is an integer that many tasks can update safely without a
    /// lock (like Go's `atomic.Int64`). Each projection yields to the runtime
    /// several times, so other tasks get to start while it's "in flight";
    /// plans with fewer drawdown years yield more, so later plans finish
    /// first. Its ending balance is the plan's drawdown years, in dollars, so
    /// a test can tell which plan a result came from.
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
            let mut result: Projection = serde_json::from_str(PROJECTION_BASIC).unwrap();
            let last = result.totals.last_mut().unwrap();
            last.balance_cents = i64::from(plan.drawdown_years) * 100;
            Ok(result)
        }
    }

    /// `count` plans with drawdown 21, 22, 23, ...
    fn plans(count: i32) -> Vec<Result<Plan, Error>> {
        (0..count)
            .map(|n| {
                let mut plan = Plan::from_json(BASIC).unwrap();
                plan.drawdown_years = 21 + n;
                Ok(plan)
            })
            .collect()
    }

    #[tokio::test]
    async fn no_more_than_four_projections_run_at_once() {
        let projector = Arc::new(CountingProjector::default());

        let results = project_all(Arc::clone(&projector), plans(13)).await;

        assert_eq!(results.len(), 13);
        assert_eq!(projector.calls.load(Ordering::SeqCst), 13);
        // Exactly 4: the limit holds, *and* they really did run concurrently.
        assert_eq!(projector.max_in_flight.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn results_stay_in_input_order_when_tasks_finish_out_of_order() {
        let results = project_all(Arc::new(CountingProjector::default()), plans(8)).await;

        // Each result's ending balance is its own plan's drawdown years, even
        // though higher drawdowns finished first.
        let endings: Vec<i64> = results
            .into_iter()
            .map(|result| result.unwrap().ending_balance)
            .collect();
        assert_eq!(endings, [21, 22, 23, 24, 25, 26, 27, 28]);
    }

    #[tokio::test]
    async fn errors_pass_through_without_projecting() {
        let projector = Arc::new(CountingProjector::default());
        let mut inputs = plans(3);
        inputs[1] = Err(Error::Output);

        let results = project_all(Arc::clone(&projector), inputs).await;

        assert!(matches!(results[1], Err(Error::Output)));
        assert!(results[0].is_ok() && results[2].is_ok());
        assert_eq!(projector.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_empty_batch_is_empty() {
        let results = project_all(Arc::new(CountingProjector::default()), Vec::new()).await;
        assert!(results.is_empty());
    }
}
