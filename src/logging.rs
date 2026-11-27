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
    //!
    //! `tracing` caches, per log statement and for the whole process, whether
    //! any subscriber wants it. With a separate subscriber per test thread
    //! (`set_default`), tests running in parallel race on that cache, and a
    //! capturing test can miss lines: it failed about half the time when run
    //! with only the server tests.
    //!
    //! So the test binary has **one** global subscriber, installed once, with
    //! this crate always enabled (no cache can say "never"). Each capturing
    //! test registers a buffer and a filter for its own thread, and the
    //! subscriber's writer sends each line to the current thread's buffer if
    //! the filter allows it. Tests run their server on tokio's single-threaded
    //! test runtime, so its tasks log on the test's thread too.

    use std::cell::RefCell;
    use std::io::Write;
    use std::sync::{Arc, Mutex, Once};

    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::fmt::MakeWriter;

    /// A shared, growable buffer that captured log lines are written into.
    /// `Clone` shares the buffer (it's an `Arc`), so the test keeps one handle
    /// and the writer gets others.
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

    // `thread_local!` gives each thread its own copy of a value, like Go's
    // (missing) goroutine-local storage or Python's `threading.local()`.
    // `RefCell` allows changing it through a shared reference, checked at
    // runtime; fine here, since only this thread ever touches its copy.
    thread_local! {
        static ACTIVE: RefCell<Option<(Captured, Targets)>> = const { RefCell::new(None) };
    }

    /// Where one log line goes: into a test's buffer, or nowhere.
    pub enum Destination {
        Buffer(Captured),
        Discard,
    }

    impl Write for Destination {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            match self {
                Destination::Buffer(captured) => captured.write(bytes),
                Destination::Discard => Ok(bytes.len()),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The global subscriber's writer: picks the destination per line.
    struct ToCurrentThread;

    // `impl<'a> MakeWriter<'a>` reads "for any lifetime `'a`": the trait is
    // generic over the lifetime of the writer it hands out.
    impl<'a> MakeWriter<'a> for ToCurrentThread {
        type Writer = Destination;

        fn make_writer(&'a self) -> Destination {
            Destination::Discard
        }

        // Called with each event's metadata (level, target), so the
        // per-test filter can decide.
        fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Destination {
            ACTIVE.with(|active| match &*active.borrow() {
                Some((captured, filter)) if filter.would_enable(meta.target(), meta.level()) => {
                    Destination::Buffer(captured.clone())
                }
                _ => Destination::Discard,
            })
        }
    }

    /// Stops capturing for this thread when dropped.
    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.borrow_mut().take());
        }
    }

    /// Captures what this thread logs, as `filter` allows (`RUST_LOG`-style
    /// target syntax such as `financials_mcp=debug`), until the returned
    /// guard is dropped.
    pub fn start(filter: &str) -> (Captured, Guard) {
        // `Once` runs its closure exactly once per process, even if many
        // tests call `start` at the same moment.
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new("financials_mcp=trace"))
                .with_writer(ToCurrentThread)
                .with_ansi(false)
                // No timestamps: their digits could match an amount by chance.
                .without_time()
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("only this module installs a global subscriber in tests");
        });

        let captured = Captured::default();
        let filter: Targets = filter.parse().expect("a valid log filter");
        ACTIVE.with(|active| *active.borrow_mut() = Some((captured.clone(), filter)));
        (captured, Guard)
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
