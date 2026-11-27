//! Runs `financials-mcp serve` as a real child process and speaks raw MCP
//! (JSON-RPC, one message per line) over its stdin and stdout, the way Claude
//! Desktop or Claude Code would.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_financials-mcp");

#[test]
fn serve_speaks_mcp_over_stdio_and_nothing_else() {
    let workdir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BINARY)
        .arg("serve")
        .current_dir(workdir.path())
        .env_remove("FINANCIALS_API_URL")
        .env_remove("FINANCIALS_ACCESS_TOKEN")
        .env_remove("FINANCIALS_MCP_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve should start");

    // `take()` moves the pipe out of the `Option` the child keeps it in, so
    // this code owns it (and can drop it later to close stdin).
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut read_message = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<serde_json::Value>(&line)
            .unwrap_or_else(|_| panic!("stdout line is not JSON-RPC: {line:?}"))
    };

    let messages = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test-client","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    ];
    writeln!(stdin, "{}", messages[0]).unwrap();
    let initialized = read_message();
    assert_eq!(initialized["id"], 1);
    assert_eq!(
        initialized["result"]["serverInfo"]["name"],
        "financials-mcp"
    );

    writeln!(stdin, "{}", messages[1]).unwrap();
    writeln!(stdin, "{}", messages[2]).unwrap();
    let tools = read_message();
    assert_eq!(tools["id"], 2);
    // Tools come back in no particular order, so sort the names.
    let listed = tools["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = listed
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "compare_scenarios",
            "find_max_spending",
            "run_projection",
            "sweep",
            "withdrawal_order_options"
        ]
    );
    for tool in listed {
        assert_eq!(
            tool["annotations"]["readOnlyHint"], true,
            "{}",
            tool["name"]
        );
    }

    // Closing stdin is how a client disconnects; the server should then exit
    // cleanly and have printed nothing else to stdout.
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "exit: {}", output.status);
    let rest = String::from_utf8(output.stdout).unwrap();
    assert!(
        rest.trim().is_empty(),
        "unexpected stdout after the session: {rest:?}"
    );
}

#[test]
fn serve_logs_tool_calls_to_stderr_and_keeps_stdout_for_the_protocol() {
    let stderr = serve_one_failing_call("debug");
    assert!(stderr.contains("projection finished"), "{stderr}");

    // A value that tries to raise rmcp's level is rejected, with a warning,
    // and still nothing private is logged.
    let stderr = serve_one_failing_call("trace,rmcp=trace");
    assert!(
        stderr.contains("FINANCIALS_MCP_LOG must be one of"),
        "{stderr}"
    );
    assert!(!stderr.contains("projection finished"), "{stderr}");
}

/// Runs `serve` with `FINANCIALS_MCP_LOG=log_level`, makes one tool call that
/// fails (nothing listens at the API URL), checks stdout and the logged
/// fields, and returns stderr.
fn serve_one_failing_call(log_level: &str) -> String {
    let workdir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BINARY)
        .arg("serve")
        .current_dir(workdir.path())
        // Nothing listens on port 9, so the projection fails fast as an
        // `api` error: a tool call with a failure to log, and no real API.
        .env("FINANCIALS_API_URL", "http://127.0.0.1:9")
        .env("FINANCIALS_ACCESS_TOKEN", "tok-SECRET-4242")
        .env("FINANCIALS_MCP_LOG", log_level)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve should start");

    let plan: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/plan-household.json")).unwrap();
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "run_projection", "arguments": {"plan": plan}}
    });
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut read_line = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        line
    };

    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"test-client","version":"0"}}}}}}"#
    )
    .unwrap();
    read_line();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    writeln!(stdin, "{call}").unwrap();
    let response: serde_json::Value = serde_json::from_str(&read_line()).unwrap();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["isError"], true);

    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "exit: {}", output.status);
    // Nothing else reached stdout: no log line slipped onto the protocol.
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("serving MCP over stdio"), "{stderr}");
    assert!(
        stderr.contains(r#"tool_call{tool="run_projection"}"#),
        "{stderr}"
    );
    assert!(stderr.contains(r#"error="api""#), "{stderr}");
    assert!(stderr.contains("projections=1"), "{stderr}");
    for private in [
        "Person A",
        "Example",
        "person-a",
        "30000000",
        "SECRET",
        "127.0.0.1",
    ] {
        assert!(
            !stderr.contains(private),
            "stderr contains {private:?}:\n{stderr}"
        );
    }
    stderr
}

/// The token the HTTP tests start the server with.
#[cfg(feature = "http")]
const HTTP_TOKEN: &str = "integration-test-token-0123456789abcdef";

/// `serve --http` with no token refuses to start, before listening.
#[cfg(feature = "http")]
#[test]
fn serve_http_refuses_to_start_without_a_token() {
    let workdir = tempfile::tempdir().unwrap();
    let output = Command::new(BINARY)
        .args(["serve", "--http", "--listen", "127.0.0.1:0"])
        .current_dir(workdir.path())
        .env_remove("MCP_BEARER_TOKEN")
        .env_remove("FINANCIALS_API_URL")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("MCP_BEARER_TOKEN is required for serve --http"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("serving MCP"),
        "it never started: {stderr}"
    );
}

/// `serve --http` with the real binary: it picks a free port (`:0`), says
/// which in its startup log line, and answers MCP over HTTP. Only built with
/// the `http` feature.
#[cfg(feature = "http")]
#[tokio::test]
async fn serve_http_answers_mcp_over_http_and_logs_to_stderr() {
    use std::sync::{Arc, Mutex};

    let workdir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BINARY)
        .args(["serve", "--http", "--listen", "127.0.0.1:0"])
        .current_dir(workdir.path())
        // Nothing listens on port 9, so a projection fails fast as `api`.
        .env("FINANCIALS_API_URL", "http://127.0.0.1:9")
        .env("MCP_BEARER_TOKEN", HTTP_TOKEN)
        .env_remove("MCP_ALLOWED_HOSTS")
        .env_remove("FINANCIALS_ACCESS_TOKEN")
        .env_remove("FINANCIALS_MCP_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve --http should start");

    // Read stderr on its own thread, so the pipe never fills up, and find
    // the startup line with the address it chose.
    let stderr = BufReader::new(child.stderr.take().unwrap());
    let log = Arc::new(Mutex::new(String::new()));
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let reader_log = Arc::clone(&log);
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            if let Some((_, address)) = line.split_once("address=") {
                let _ = address_tx.send(address.trim().to_string());
            }
            reader_log.lock().unwrap().push_str(&line);
            reader_log.lock().unwrap().push('\n');
        }
    });
    let address = address_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a startup line with the listen address");
    let url = format!("http://{address}/mcp");

    // Without the token, nothing gets through.
    let unauthorized = reqwest::Client::new()
        .post(&url)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);

    let post = |body: String| {
        reqwest::Client::new()
            .post(&url)
            .bearer_auth(HTTP_TOKEN)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body)
            .send()
    };
    let initialize = post(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#
            .to_string(),
    )
    .await
    .unwrap();
    assert_eq!(initialize.status(), 200);
    let info: serde_json::Value = initialize.json().await.unwrap();
    assert_eq!(info["result"]["serverInfo"]["name"], "financials-mcp");

    let plan: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/plan-household.json")).unwrap();
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "run_projection", "arguments": {"plan": plan}}
    });
    let response: serde_json::Value = post(call.to_string()).await.unwrap().json().await.unwrap();
    assert_eq!(response["result"]["isError"], true, "{response}");

    child.kill().unwrap();
    child.wait().unwrap();
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(stdout.is_empty(), "nothing goes to stdout: {stdout}");

    let log = log.lock().unwrap().clone();
    assert!(log.contains("serving MCP over HTTP at /mcp"), "{log}");
    assert!(log.contains(r#"tool_call{tool="run_projection"}"#), "{log}");
    assert!(log.contains(r#"error="api""#), "{log}");
    assert!(log.contains("request rejected: auth_failed"), "{log}");
    for private in ["Person A", "Example", "person-a", "30000000", HTTP_TOKEN] {
        assert!(
            !log.contains(private),
            "stderr contains {private:?}:\n{log}"
        );
    }
}
