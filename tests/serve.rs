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
    assert_eq!(names, ["compare_scenarios", "run_projection"]);
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
