//! The app's error type.
//!
//! Rust has no exceptions. A function that can fail returns
//! `Result<T, Error>`, and the caller must handle the `Err` case: match on
//! it, pass it up with `?`, or deliberately stop with `expect`. This module
//! defines the `Error` enum those results carry.
//!
//! **Privacy rule:** error messages name *where* a problem is (a field path
//! such as `items[2].amountCents`) and *what* rule it breaks, but never quote
//! a value from the plan. Plans hold private financial data, and errors end up
//! in model conversations and logs.

/// Everything that can go wrong in this crate.
///
/// A Rust `enum` is a *sum type*: an `Error` is exactly one of these variants,
/// and each variant carries its own fields. Go would use several error types
/// plus `errors.As`; Python would use exception subclasses.
///
/// `#[derive(thiserror::Error)]` generates the standard error-trait
/// implementations, and each `#[error("...")]` attribute becomes that
/// variant's `Display` text (what `to_string()` and `{}` print). `{line}`
/// refers to the variant's field.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The text isn't valid JSON (or stops early).
    #[error("plan is not valid JSON (line {line}, column {column})")]
    InvalidJson { line: usize, column: usize },

    /// Valid JSON, but not the shape of a plan: a missing or unknown field, or
    /// a value of the wrong type.
    #[error("plan has a problem at {location}: {problem}")]
    PlanShape { location: String, problem: String },

    /// A well-formed plan that breaks a rule financials-api would reject.
    /// Every problem is listed, as the API does, so they can all be fixed at
    /// once. `.problems.join("; ")` is ordinary Rust code inside the message.
    #[error("invalid plan: {}", .problems.join("; "))]
    InvalidPlan { problems: Vec<String> },

    /// financials-api refused the access token (`401`). The token itself is
    /// never part of the message.
    #[error(
        "financials-api rejected the access token (missing or invalid); check FINANCIALS_ACCESS_TOKEN"
    )]
    Auth,

    /// financials-api couldn't be reached or failed: connection refused,
    /// timeout, a server error, a redirect, or a wrong URL.
    #[error("financials-api is unavailable: {problem}")]
    Api { problem: String },

    /// financials-api answered with something that breaks its own contract,
    /// such as a projection with no years. This points to a bug or a version
    /// mismatch, not a bad plan.
    #[error("unexpected response from financials-api: {problem}")]
    UnexpectedResponse { problem: String },

    /// A plan file couldn't be read. Names the path, never the contents.
    #[error("{problem}")]
    PlanFile { problem: String },

    /// The result couldn't be written (for example, stdout was closed).
    #[error("could not write the output")]
    Output,

    /// The MCP server couldn't start or stopped unexpectedly.
    #[error("MCP server problem: {problem}")]
    Server { problem: String },

    /// A setting is missing, malformed or unsafe. Messages name the variable
    /// or `.env` line, never a value, since values may include the token.
    #[error("configuration problem: {problem}")]
    Config { problem: String },
}

impl Error {
    /// Converts a JSON parsing error into an [`Error`] without any plan values.
    ///
    /// `path` is where the problem is (`items[0].amountCents`); `serde_path_to_error`
    /// tracks it, since serde_json alone only reports a line and column.
    pub(crate) fn from_json_error(path: &str, err: &serde_json::Error) -> Error {
        use serde_json::error::Category;

        // `match` must handle every variant of `Category`, so a new kind of
        // error can't be forgotten: the code wouldn't compile.
        match err.classify() {
            Category::Syntax | Category::Eof | Category::Io => Error::InvalidJson {
                line: err.line(),
                column: err.column(),
            },
            Category::Data => Error::PlanShape {
                location: if path == "." {
                    "the top level".to_string()
                } else {
                    format!("`{path}`")
                },
                problem: describe_without_values(&err.to_string()),
            },
        }
    }
}

/// Rewrites a serde data error so it keeps the rule but drops the value.
///
/// serde's messages look like
/// `invalid type: floating point `10000.5`, expected i64 at line 4 column 35`.
/// The part between `invalid type:` and `, expected` is the offending value,
/// so it's removed; type names are made friendlier; the position is dropped
/// because the path already says where.
fn describe_without_values(message: &str) -> String {
    // `rsplit_once` returns `Option<(&str, &str)>`: `Some((before, after))`
    // if the separator is found (searching from the right), `None` if not.
    // The `match` unpacks it. `_position` is unused; a leading `_` says so.
    let message = match message.rsplit_once(" at line ") {
        Some((before, _position)) => before,
        None => message,
    };

    for kind in ["invalid type", "invalid value", "invalid length"] {
        if message.starts_with(kind) {
            // `if let` is a `match` with a single interesting case.
            if let Some((_value, expected)) = message.split_once(", expected ") {
                return format!("{kind}, expected {}", friendly_type(expected));
            }
            return kind.to_string();
        }
    }
    // Missing and unknown field messages only contain field names.
    message.to_string()
}

/// Maps serde's type names to words that make sense to a person or a model.
///
/// `match` on a string slice compares the text, like a `switch` on a string in
/// Go. The `_` arm catches everything else.
fn friendly_type(expected: &str) -> &str {
    match expected {
        "i64" | "i32" => "a whole number (cents or basis points, no decimals)",
        "a boolean" => "true or false",
        "a sequence" => "a list",
        "struct Plan"
        | "struct ItemInput"
        | "struct PersonInput"
        | "struct ExpenseInput"
        | "struct MortgageInput" => "an object",
        _ => expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_removed_from_type_errors() {
        let described = describe_without_values(
            "invalid type: floating point `10000.5`, expected i64 at line 4 column 35",
        );
        assert_eq!(
            described,
            "invalid type, expected a whole number (cents or basis points, no decimals)"
        );
        assert!(!described.contains("10000"));
    }

    #[test]
    fn string_values_are_removed_too() {
        let described = describe_without_values(
            r#"invalid type: string "my secret account", expected a boolean at line 2 column 9"#,
        );
        assert_eq!(described, "invalid type, expected true or false");
    }

    #[test]
    fn field_name_messages_are_kept_without_position() {
        assert_eq!(
            describe_without_values("missing field `name` at line 9 column 5"),
            "missing field `name`"
        );
    }

    #[test]
    fn display_text_comes_from_the_error_attributes() {
        let err = Error::InvalidPlan {
            problems: vec![
                "items are required".to_string(),
                "years must be between 1 and 75".to_string(),
            ],
        };
        assert_eq!(
            err.to_string(),
            "invalid plan: items are required; years must be between 1 and 75"
        );
    }
}
