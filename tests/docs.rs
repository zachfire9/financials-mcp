//! Keeps the documentation honest.
//!
//! Docs drift silently: a file gets renamed, a heading changes, a tool or
//! setting is added and never documented. These tests read the Markdown files
//! and check them against the repository and the real binary:
//!
//! - every relative link points to a file that exists, and every `#anchor`
//!   to a heading that exists;
//! - the docs mention every tool the server lists, every subcommand and
//!   setting in `--help`, and every source file.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BINARY: &str = env!("CARGO_BIN_EXE_financials-mcp");

/// The repository root. `CARGO_MANIFEST_DIR` is set by Cargo at compile time
/// to the folder holding `Cargo.toml`, so tests don't depend on the current
/// directory.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every Markdown file the repo maintains: the README, AGENTS.md and `docs/`.
fn markdown_files() -> Vec<PathBuf> {
    let mut files = vec![root().join("README.md"), root().join("AGENTS.md")];
    for dir in ["docs", "docs/plans"] {
        // `flatten()` skips entries that failed to read; `filter` keeps .md
        // files. A `?`-free chain like this is common in tests.
        let mut found: Vec<PathBuf> = fs::read_dir(root().join(dir))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        found.sort();
        files.extend(found);
    }
    files
}

/// The text of a file with fenced code blocks blanked out, so examples inside
/// ``` blocks aren't mistaken for links or headings.
fn prose(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap();
    let mut in_code = false;
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                in_code = !in_code;
                ""
            } else if in_code {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The targets of every `[text](target)` link in `text`.
fn link_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = text;
    // `split_once` finds the next `](`; everything up to the following `)` is
    // the target. Good enough for these docs, which have no `)` in URLs.
    while let Some((_, after)) = rest.split_once("](") {
        if let Some((target, remaining)) = after.split_once(')') {
            targets.push(target.trim().to_string());
            rest = remaining;
        } else {
            break;
        }
    }
    targets
}

/// GitHub's anchor for a heading: lowercase, spaces to `-`, and everything
/// but letters, digits, `-` and `_` removed. Repeated headings get `-1`,
/// `-2`, ... appended, which `anchors` handles.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// Every anchor a Markdown file's headings produce.
fn anchors(path: &Path) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    prose(path)
        .lines()
        .filter_map(|line| {
            let heading = line.trim_start_matches('#');
            (line.starts_with('#') && heading.starts_with(' ')).then(|| slug(heading))
        })
        .map(|base| {
            // `entry().or_insert(0)` gets the count for this slug, inserting 0
            // the first time: Go's `m[k]++` on a map that defaults to zero.
            let count = seen.entry(base.clone()).or_insert(0);
            let anchor = if *count == 0 {
                base.clone()
            } else {
                format!("{base}-{count}")
            };
            *count += 1;
            anchor
        })
        .collect()
}

#[test]
fn slugs_match_github() {
    assert_eq!(
        slug(" Step 19 — Streamable HTTP transport, locally"),
        "step-19--streamable-http-transport-locally"
    );
    assert_eq!(
        slug("Step 11: compare_scenarios"),
        "step-11-compare_scenarios"
    );
    assert_eq!(
        slug("Types: structs, enums, Option and Result"),
        "types-structs-enums-option-and-result"
    );
}

#[test]
fn every_relative_link_and_anchor_resolves() {
    let mut broken = Vec::new();
    for file in markdown_files() {
        let dir = file.parent().unwrap();
        for target in link_targets(&prose(&file)) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            // `split_once('#')` separates `path#anchor`; a target with no `#`
            // is all path.
            let (path_part, anchor) = match target.split_once('#') {
                Some((path, anchor)) => (path, Some(anchor)),
                None => (target.as_str(), None),
            };
            let resolved = if path_part.is_empty() {
                file.clone()
            } else {
                dir.join(path_part)
            };
            let name = file.strip_prefix(root()).unwrap().display().to_string();
            if !resolved.exists() {
                broken.push(format!("{name}: {target} (no such file)"));
                continue;
            }
            // A *let chain*: the `if let` and the conditions after `&&` must
            // all hold (Rust 2024, 1.88+).
            if let Some(anchor) = anchor
                && resolved.extension().is_some_and(|ext| ext == "md")
                && !anchors(&resolved).iter().any(|a| a == anchor)
            {
                broken.push(format!("{name}: {target} (no such heading)"));
            }
        }
    }
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

/// Runs `financials-mcp serve` just long enough to ask for its tool list.
fn tool_names() -> Vec<String> {
    let workdir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BINARY)
        .arg("serve")
        .current_dir(workdir.path())
        .env_remove("FINANCIALS_API_URL")
        .env_remove("FINANCIALS_ACCESS_TOKEN")
        .env("FINANCIALS_MCP_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();

    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"docs-test","version":"0"}}}}}}"#
    )
    .unwrap();
    stdout.read_line(&mut line).unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    drop(stdin);
    child.wait().unwrap();

    let list: serde_json::Value = serde_json::from_str(&line).unwrap();
    list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_docs_describe_every_tool() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let tools = fs::read_to_string(root().join("docs/tools.md")).unwrap();
    let names = tool_names();
    assert_eq!(names.len(), 5, "{names:?}");
    for name in names {
        let quoted = format!("`{name}`");
        assert!(readme.contains(&quoted), "README doesn't list {name}");
        assert!(
            tools.contains(&quoted),
            "docs/tools.md doesn't describe {name}"
        );
    }
}

/// The words in `--help` output for `args` that look like settings
/// (`FINANCIALS_…`) or, for the top level, subcommand names.
fn help(args: &[&str]) -> String {
    let output = Command::new(BINARY).args(args).output().unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn the_readme_documents_every_subcommand_and_setting() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();

    // Subcommands: the indented lines under "Commands:" in `--help`.
    let top = help(&["--help"]);
    let subcommands: Vec<&str> = top
        .lines()
        .skip_while(|line| !line.starts_with("Commands:"))
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .collect();
    assert_eq!(subcommands, ["project", "serve"]);
    for name in subcommands {
        assert!(
            readme.contains(&format!("financials-mcp {name}"))
                || readme.contains(&format!("-- {name}")),
            "README doesn't show the {name} command"
        );
    }

    // Settings: every FINANCIALS_* or MCP_* variable mentioned in any
    // `--help` must be in the README's configuration table.
    let mut settings: Vec<String> = ["project", "serve"]
        .iter()
        .flat_map(|cmd| {
            help(&[cmd, "--help"])
                .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                .filter(|word| word.starts_with("FINANCIALS_") || word.starts_with("MCP_"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    settings.sort();
    settings.dedup();
    let mut expected = vec![
        "FINANCIALS_ACCESS_TOKEN",
        "FINANCIALS_API_URL",
        "FINANCIALS_MCP_LOG",
    ];
    // The HTTP settings only appear in `--help` when `serve --http` exists.
    if cfg!(feature = "http") {
        expected.extend(["MCP_ALLOWED_HOSTS", "MCP_BEARER_TOKEN"]);
    }
    assert_eq!(settings, expected);
    for setting in settings {
        assert!(
            readme.contains(&format!("| `{setting}` |")),
            "README's configuration table is missing {setting}"
        );
    }
}

#[test]
fn every_source_file_is_in_the_layout_and_the_module_table() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let architecture = fs::read_to_string(root().join("docs/architecture.md")).unwrap();
    let rust_files = |dir: &str| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root().join(dir))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".rs"))
            .collect();
        names.sort();
        names
    };

    for name in rust_files("src") {
        assert!(
            readme.contains(&name),
            "README's layout is missing src/{name}"
        );
        assert!(
            architecture.contains(&format!("(../src/{name})")),
            "docs/architecture.md's module table is missing src/{name}"
        );
    }
    for name in rust_files("tests") {
        assert!(
            readme.contains(&name),
            "README's layout is missing tests/{name}"
        );
    }
}
