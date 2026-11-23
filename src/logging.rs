//! **Logging without plan contents.**
//!
//! Each tool call logs one line at `info`: the tool name, how long it took,
//! how many projections it ran, and the error *kind* if it failed. Never an
//! amount, a name, an id, an error message or a token. Each projection also
//! logs a `debug` line with its duration and result kind.
//!
//! Logs go to **stderr**: in stdio mode, stdout carries the MCP protocol, and
//! one stray line there would corrupt it.
//!
//! # How `tracing` works
//!
//! `tracing` separates *producing* log records from *handling* them, like
//! Go's `log/slog` (a `Logger` and a `Handler`) or Python's `logging`
//! (loggers and handlers):
//!
//! - **Events** (`info!`, `debug!`, ...) are single records with structured
//!   fields: `info!(tool = "sweep", projections = 4, "tool call finished")`.
//! - **Spans** mark a stretch of work. Events inside a span carry its fields
//!   too, so every line from one tool call is tagged with `tool=...`.
//! - A **subscriber** decides what's kept and where it goes. [`init`]
//!   installs one that filters by level and writes text lines to stderr.
//!
//! If no subscriber is installed (as in most tests), events cost almost
//! nothing and go nowhere.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use tracing_subscriber::EnvFilter;

use crate::error::Error;
use crate::plan::Plan;
use crate::projection::Projection;
use crate::projector::Projector;

/// The environment variable that sets this crate's log level: `error`,
/// `warn`, `info` (the default), `debug`, `trace` or `off`.
pub const LOG_ENV: &str = "FINANCIALS_MCP_LOG";

/// The level for every other crate, fixed. At `debug`, rmcp logs every
/// request and result in full (plans included) and reqwest logs the API
/// host; at `warn` they log neither. So only a *level* is accepted, never a
/// `RUST_LOG`-style list of targets that could raise them, and `RUST_LOG`
/// itself is ignored (a global `RUST_LOG=debug` set for another program
/// would otherwise leak plans here).
const DEPENDENCY_LEVEL: &str = "warn";

/// The filter used when `FINANCIALS_MCP_LOG` is unset.
pub const DEFAULT_FILTER: &str = "warn,financials_mcp=info";

/// The filter for a `FINANCIALS_MCP_LOG` value: this crate at the given level,
/// everything else at `warn`. Returns the default for an unset, empty or
/// unrecognized value, plus whether the value was rejected.
///
/// A pure function (no environment access), so it's easy to test.
pub fn filter_for(value: Option<&str>) -> (String, bool) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return (DEFAULT_FILTER.to_string(), false);
    };
    // `LevelFilter` parses `info`, `DEBUG`, `off` and so on, and rejects
    // anything else, including `rmcp=debug` or `debug,rmcp=trace`.
    match value.parse::<tracing_subscriber::filter::LevelFilter>() {
        Ok(level) => (format!("{DEPENDENCY_LEVEL},financials_mcp={level}"), false),
        Err(_) => (DEFAULT_FILTER.to_string(), true),
    }
}

/// Installs the stderr logger. Call once, at startup, before serving.
///
/// An unrecognized `FINANCIALS_MCP_LOG` falls back to the default with a
/// warning rather than failing: logging should never stop the server.
pub fn init() {
    let value = std::env::var(LOG_ENV).ok();
    let (filter, rejected) = filter_for(value.as_deref());

    // The builder is configured with method calls, then `init()` installs it
    // as the global subscriber. `with_writer(std::io::stderr)` is the
    // important line: stdout belongs to the protocol. ANSI colors are off
    // because MCP clients usually write stderr to a log file.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .init();

    if rejected {
        tracing::warn!(
            "{LOG_ENV} must be one of error, warn, info, debug, trace or off; using info"
        );
    }
}

/// A projector wrapper that counts projections and logs each one at `debug`.
///
/// Each tool call wraps the server's projector in a fresh `Counted`, so its
/// count belongs to that call alone, even while other calls run. It
/// implements [`Projector`] itself, so every tool (generic over `P:
/// Projector`) accepts it unchanged: this is the payoff of making the tools
/// generic back in Step 06.
pub struct Counted<P: Projector> {
    inner: Arc<P>,
    // `AtomicU32` rather than a plain `u32`: `project` takes `&self`, and
    // `compare_scenarios` calls it from several tasks at once.
    count: AtomicU32,
}

impl<P: Projector> Counted<P> {
    pub fn new(inner: Arc<P>) -> Counted<P> {
        Counted {
            inner,
            count: AtomicU32::new(0),
        }
    }

    /// Projections started so far.
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::Relaxed)
    }
}

impl<P: Projector> Projector for Counted<P> {
    async fn project(&self, plan: &Plan) -> Result<Projection, Error> {
        self.count.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let result = self.inner.project(plan).await;
        // `%` records a field with its `Display`, `?` with its `Debug`; a bare
        // value must implement tracing's `Value` trait (numbers, `&str`,
        // `bool`). Only fixed strings and numbers are logged here.
        tracing::debug!(
            duration_ms = elapsed_ms(started),
            outcome = match &result {
                Ok(_) => "ok",
                Err(err) => err.kind(),
            },
            "projection finished"
        );
        result
    }
}

/// Milliseconds since `started`, as a whole number for the logs.
pub fn elapsed_ms(started: Instant) -> u64 {
    // `as_millis` returns `u128`; `try_from` keeps it honest, and
    // `unwrap_or(u64::MAX)` covers the (585-million-year) overflow.
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Logs the one `info` line for a finished tool call.
pub fn tool_call_finished(started: Instant, projections: u32, error: Option<&Error>) {
    match error {
        None => tracing::info!(
            duration_ms = elapsed_ms(started),
            projections,
            "tool call succeeded"
        ),
        Some(err) => tracing::info!(
            duration_ms = elapsed_ms(started),
            projections,
            error = err.kind(),
            "tool call failed"
        ),
    }
}

#[cfg(test)]
pub(crate) mod capture {
    //! Collecting log output in memory for tests.

    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    /// A shared, growable buffer that a test subscriber writes into.
    ///
    /// `MakeWriter` is the subscriber's way of asking "where should this line
    /// go?"; here, always into the same buffer. `Clone` shares the buffer
    /// (it's an `Arc`), so the test keeps one handle and the subscriber the
    /// other.
    #[derive(Clone, Default)]
    pub struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        pub fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // `impl<'a> MakeWriter<'a> for Captured` reads "for any lifetime `'a`":
    // the trait is generic over the lifetime of the writer it hands out.
    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Captures everything logged on this thread at `filter` until the
    /// returned guard is dropped. Tests run the server on tokio's
    /// single-threaded test runtime, so its tasks log on this thread too.
    pub fn start(filter: &str) -> (Captured, tracing::subscriber::DefaultGuard) {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(captured.clone())
            .with_ansi(false)
            // No timestamps: their digits could match an amount by chance.
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (captured, guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projector::FakeProjector;

    #[test]
    fn only_a_level_is_accepted_and_dependencies_stay_at_warn() {
        assert_eq!(filter_for(None), (DEFAULT_FILTER.to_string(), false));
        assert_eq!(filter_for(Some("  ")), (DEFAULT_FILTER.to_string(), false));
        assert_eq!(
            filter_for(Some("debug")),
            ("warn,financials_mcp=debug".to_string(), false)
        );
        assert_eq!(
            filter_for(Some("TRACE")),
            ("warn,financials_mcp=trace".to_string(), false)
        );
        assert_eq!(
            filter_for(Some("off")),
            ("warn,financials_mcp=off".to_string(), false)
        );
        // Anything that could raise another crate's level is rejected.
        for unsafe_value in [
            "rmcp=debug",
            "debug,rmcp=trace",
            "trace,reqwest=debug",
            "verbose",
        ] {
            assert_eq!(
                filter_for(Some(unsafe_value)),
                (DEFAULT_FILTER.to_string(), true),
                "{unsafe_value}"
            );
        }
    }

    const PLAN: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION: &str = include_str!("../tests/fixtures/projection-basic.json");

    #[tokio::test]
    async fn counted_counts_and_logs_each_projection_at_debug() {
        let fake = FakeProjector::scripted(vec![
            Ok(serde_json::from_str(PROJECTION).unwrap()),
            Err(Error::Auth),
        ]);
        let counted = Counted::new(Arc::new(fake));
        let plan = Plan::from_json(PLAN).unwrap();
        let (captured, _guard) = capture::start("financials_mcp=debug");

        assert!(counted.project(&plan).await.is_ok());
        assert!(counted.project(&plan).await.is_err());

        assert_eq!(counted.count(), 2);
        let logs = captured.text();
        assert!(logs.contains("projection finished"), "{logs}");
        assert!(logs.contains("outcome=\"ok\""), "{logs}");
        assert!(logs.contains("outcome=\"auth\""), "{logs}");
    }

    #[test]
    fn every_error_kind_is_a_fixed_snake_case_word() {
        let samples = [
            Error::InvalidJson { line: 1, column: 1 },
            Error::Auth,
            Error::Api {
                problem: "secret 123".into(),
            },
            Error::InvalidOverride {
                index: 0,
                op: "set_item",
                problem: "no item with id `person-a`".into(),
            },
        ];
        for err in samples {
            let kind = err.kind();
            assert!(
                kind.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{kind}"
            );
        }
    }
}
