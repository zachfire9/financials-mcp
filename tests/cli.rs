//! Integration tests: each file in `tests/` is compiled as its own crate and
//! can only use the library's public API, or run the built binary, exactly as
//! a user would.

use std::process::Command;

#[test]
fn running_the_binary_prints_the_banner() {
    // Cargo builds the binary before integration tests run and tells the test
    // where it is through this compile-time environment variable.
    let output = Command::new(env!("CARGO_BIN_EXE_financials-mcp"))
        .output()
        .expect("the financials-mcp binary should run");

    assert!(
        output.status.success(),
        "binary exited with {}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert_eq!(stdout.trim_end(), financials_mcp::banner());
}
