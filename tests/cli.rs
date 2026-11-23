//! Integration tests: each file in `tests/` is compiled as its own crate and
//! can only use the library's public API, or run the built binary, exactly as
//! a user would.

use std::process::{Command, Output};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BINARY: &str = env!("CARGO_BIN_EXE_financials-mcp");
const PLAN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/plan-basic.json"
);
const PROJECTION: &str = include_str!("fixtures/projection-basic.json");

/// Runs the binary with an empty working directory (so no stray `.env` is
/// read) and a controlled environment.
fn run(args: &[&str], api_url: Option<&str>) -> Output {
    let workdir = tempfile::tempdir().unwrap();
    let mut command = Command::new(BINARY);
    command
        .args(args)
        .current_dir(workdir.path())
        .env_remove("FINANCIALS_API_URL")
        .env_remove("FINANCIALS_ACCESS_TOKEN");
    if let Some(url) = api_url {
        command.env("FINANCIALS_API_URL", url);
    }
    command
        .output()
        .expect("the financials-mcp binary should run")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("output should be UTF-8")
}

#[test]
fn running_the_binary_prints_the_banner() {
    let output = run(&[], None);

    assert!(
        output.status.success(),
        "binary exited with {}",
        output.status
    );
    assert_eq!(text(&output.stdout).trim_end(), financials_mcp::banner());
}

#[test]
fn help_describes_the_project_command() {
    let output = run(&["--help"], None);
    assert!(output.status.success());
    assert!(
        text(&output.stdout).contains("project"),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn usage_mistakes_exit_with_code_2() {
    let output = run(&["project"], None);
    assert_eq!(
        output.status.code(),
        Some(2),
        "clap's usage-error exit code"
    );
}

#[test]
fn a_missing_plan_file_exits_with_code_1() {
    let output = run(&["project", "does-not-exist.json"], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "nothing on stdout when it fails");
    assert_eq!(
        text(&output.stderr).trim_end(),
        "error: plan file not found: does-not-exist.json"
    );
}

#[tokio::test]
async fn project_prints_the_summary_from_the_api() {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/projections"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PROJECTION))
        .expect(1)
        .mount(&api)
        .await;
    let url = api.uri();

    // Waiting for a child process *blocks* the thread, and blocking a tokio
    // runtime thread stalls every other task scheduled on it. `spawn_blocking`
    // runs the blocking work on a separate thread pool and gives back a future
    // to `.await`, keeping the runtime free. (Here it happens not to matter:
    // wiremock runs its server on its own background thread, so a plain
    // blocking call would also pass. It's still the habit to build.)
    let output = tokio::task::spawn_blocking(move || run(&["project", PLAN], Some(&url)))
        .await
        .unwrap();

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["depletionYear"], 36);
    assert_eq!(summary["yearsShort"], 5);
}

#[tokio::test]
async fn an_unreachable_api_exits_with_code_1_and_a_clear_message() {
    let output =
        tokio::task::spawn_blocking(|| run(&["project", PLAN], Some("http://127.0.0.1:9")))
            .await
            .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("could not connect to financials-api"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn help_text_is_written_for_users_with_no_implementation_notes() {
    for args in [
        &["--help"][..],
        &["project", "--help"],
        &["serve", "--help"],
    ] {
        let output = Command::new(BINARY).args(args).output().unwrap();
        let help = text(&output.stdout);
        assert!(output.status.success(), "{args:?}");
        for leak in ["///", "Rust", "clap", "`Option`", "derive"] {
            assert!(
                !help.contains(leak),
                "{args:?} help mentions {leak:?}:\n{help}"
            );
        }
    }
}
