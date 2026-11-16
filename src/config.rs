//! Configuration: where financials-api is and how to authenticate to it.
//!
//! Settings come from three places, highest priority first, the same as in
//! financials-api and agent-harness:
//!
//! 1. the process environment (a variable that is set always wins, even when
//!    empty; empty means "use the default");
//! 2. a local `.env` file (gitignored; `.env.example` shows the format);
//! 3. built-in defaults.
//!
//! | Variable                  | Default                 | Meaning                                  |
//! |---------------------------|-------------------------|------------------------------------------|
//! | `FINANCIALS_API_URL`      | `http://localhost:8080` | Base URL of financials-api               |
//! | `FINANCIALS_ACCESS_TOKEN` | *(none)*                | Sent as `X-Financials-Access-Token` if set |
//!
//! The access token is a secret: it is never printed, not even by `{:?}`.

use std::collections::HashMap;
use std::fmt;

use crate::error::Error;

pub const API_URL_VAR: &str = "FINANCIALS_API_URL";
pub const ACCESS_TOKEN_VAR: &str = "FINANCIALS_ACCESS_TOKEN";
pub const DEFAULT_API_URL: &str = "http://localhost:8080";

/// Settings for talking to financials-api.
///
/// `#[derive(Debug)]` works here even though `access_token` is secret,
/// because `AccessToken` has its own hand-written `Debug` (below) that hides
/// the value. Deriving a trait on a struct uses each field's implementation.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Base URL without a trailing slash, for example `http://localhost:8080`.
    pub api_url: String,
    /// The API access token, if one is configured.
    pub access_token: Option<AccessToken>,
}

/// A secret token that never shows up in output.
///
/// This is the **newtype pattern**: a struct with a single field that wraps
/// another type (here a `String`) to give it different behavior. The field is
/// private (no `pub`), so the only ways to read the value are
/// [`AccessToken::expose`] or a deliberate `.0` inside this module. The
/// compiler then stops the token being formatted by accident: there's no
/// `Display` implementation, so `format!("{token}")` won't compile, and the
/// hand-written `Debug` prints `<redacted>`.
#[derive(Clone, PartialEq)]
pub struct AccessToken(String);

impl AccessToken {
    /// Returns the token for the one place that needs it: the HTTP header.
    /// The name makes every use easy to spot in review.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Implementing a trait by hand.
///
/// `Debug` is what `{:?}` uses. A trait is a set of methods a type promises
/// to provide, like a Go interface, except that in Rust the implementation is
/// always written explicitly with `impl Trait for Type`. Go interfaces are
/// satisfied implicitly by having the right methods.
impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

impl Config {
    /// Loads configuration from the real process environment and `./.env`.
    ///
    /// This is the only function that touches global state; everything else
    /// takes its inputs as arguments so tests don't have to modify the real
    /// environment (which every test in the process shares).
    pub fn from_env() -> Result<Config, Error> {
        let dotenv = match std::fs::read_to_string(".env") {
            Ok(text) => Some(text),
            // A missing `.env` is normal: settings may all come from the
            // environment or the defaults.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            // `if` after a pattern is a *match guard*: this arm only applies
            // when the condition holds, so any other read error falls through.
            Err(_) => {
                return Err(Error::Config {
                    problem: "could not read .env".to_string(),
                });
            }
        };
        // `|key| std::env::var(key).ok()` is a closure: a small anonymous
        // function, like a Python lambda or a Go func literal. `.ok()` turns
        // the `Result` from `env::var` into an `Option`.
        Config::from_sources(|key| std::env::var(key).ok(), dotenv.as_deref())
    }

    /// Builds the configuration from explicit sources.
    ///
    /// `env` looks up a process environment variable. Its type, `impl Fn(&str)
    /// -> Option<String>`, means "any function or closure that takes a `&str`
    /// and returns an `Option<String>`". Tests pass a closure over a small
    /// map instead of the real environment.
    ///
    /// `dotenv` is the text of a `.env` file, if there is one.
    pub fn from_sources(
        env: impl Fn(&str) -> Option<String>,
        dotenv: Option<&str>,
    ) -> Result<Config, Error> {
        let file_values = match dotenv {
            Some(text) => parse_dotenv(text)?,
            None => HashMap::new(),
        };

        let api_url = value_for(API_URL_VAR, &env, &file_values)
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());
        let api_url = check_api_url(&api_url)?;

        // `.map(AccessToken)` uses the newtype's name as a function: a tuple
        // struct's name is also its constructor, `AccessToken(String)`.
        let access_token = value_for(ACCESS_TOKEN_VAR, &env, &file_values).map(AccessToken);

        Ok(Config {
            api_url,
            access_token,
        })
    }
}

/// Looks a setting up in the environment, then the `.env` values.
///
/// Returns `None` when the setting is unset *or* empty, so the caller's
/// default applies. A variable set in the environment wins over `.env` even
/// when it's empty, matching financials-api.
fn value_for(
    key: &str,
    env: &impl Fn(&str) -> Option<String>,
    file_values: &HashMap<String, String>,
) -> Option<String> {
    let value = match env(key) {
        Some(value) => value,
        // `HashMap::get` returns `Option<&String>` (a reference into the
        // map); `.cloned()` turns it into an owned `Option<String>`.
        None => file_values.get(key).cloned()?,
    };
    let value = value.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// Parses `.env` text: `KEY=VALUE` lines, `#` comments and blank lines.
///
/// Same rules as financials-api: keys and values are trimmed, and one layer of
/// surrounding quotes is removed. Errors give the line number only, never the
/// line, because a malformed line may contain the token.
fn parse_dotenv(text: &str) -> Result<HashMap<String, String>, Error> {
    let mut values = HashMap::new();
    // `.lines()` iterates over the text line by line without copying it;
    // each `line` is a `&str` borrowed from `text`.
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            // `let ... else` destructures when the pattern matches and runs
            // the `else` block (which must leave the function or loop) when
            // it doesn't: a neat way to write "unwrap or bail out".
            return Err(Error::Config {
                problem: format!(".env line {line_number}: expected KEY=VALUE"),
            });
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::Config {
                problem: format!(".env line {line_number}: key is required"),
            });
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

/// Checks the API URL and removes any trailing slash.
///
/// Plain `http://` is only allowed for this machine (`localhost`, `127.0.0.1`,
/// `[::1]`), so the access token is never sent unencrypted over a network.
fn check_api_url(url: &str) -> Result<String, Error> {
    let invalid = |problem: &str| Error::Config {
        problem: format!("{API_URL_VAR} {problem}"),
    };

    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err(invalid("must start with http:// or https://"));
    };

    if url.chars().any(char::is_whitespace) {
        return Err(invalid("must not contain spaces"));
    }
    // The host is everything up to the first `/`, `?` or `#`.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(invalid(&format!(
            "must not contain credentials; use {ACCESS_TOKEN_VAR}"
        )));
    }
    let host = host_of(authority);
    if host.is_empty() {
        return Err(invalid("must include a host"));
    }
    if scheme == "http" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return Err(invalid("must use https:// unless it points at localhost"));
    }

    Ok(url.trim_end_matches('/').to_string())
}

/// The host part of `host:port`, keeping IPv6 brackets: `[::1]:8080` →
/// `[::1]`.
fn host_of(authority: &str) -> &str {
    if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => &authority[..=end],
            None => authority,
        }
    } else {
        // `split(':').next()` always yields at least one piece, so `unwrap_or`
        // is just belt and braces.
        authority.split(':').next().unwrap_or(authority)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake environment: a closure that looks keys up in a fixed list.
    ///
    /// `move` makes the closure take ownership of the map it builds, so it can
    /// outlive this function. `+ use<>` tells the compiler the returned
    /// closure borrows *nothing* from the arguments. Without it, Rust 2024
    /// assumes an `impl Trait` return may still borrow `pairs`, and rejects
    /// callers that pass a temporary list (the error suggests exactly this).
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn no_env() -> impl Fn(&str) -> Option<String> + use<> {
        env_of(&[])
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let config = Config::from_sources(no_env(), None).unwrap();
        assert_eq!(config.api_url, "http://localhost:8080");
        assert_eq!(config.access_token, None);
    }

    #[test]
    fn dotenv_overrides_defaults() {
        let dotenv = "# local settings\nFINANCIALS_API_URL=https://api.example.test/\n\nFINANCIALS_ACCESS_TOKEN=\"from-file\"\n";
        let config = Config::from_sources(no_env(), Some(dotenv)).unwrap();
        assert_eq!(config.api_url, "https://api.example.test");
        assert_eq!(config.access_token.unwrap().expose(), "from-file");
    }

    #[test]
    fn environment_overrides_dotenv() {
        let env = env_of(&[
            ("FINANCIALS_API_URL", "http://127.0.0.1:9000"),
            ("FINANCIALS_ACCESS_TOKEN", "from-env"),
        ]);
        let dotenv =
            "FINANCIALS_API_URL=https://api.example.test\nFINANCIALS_ACCESS_TOKEN=from-file";
        let config = Config::from_sources(env, Some(dotenv)).unwrap();
        assert_eq!(config.api_url, "http://127.0.0.1:9000");
        assert_eq!(config.access_token.unwrap().expose(), "from-env");
    }

    #[test]
    fn empty_environment_value_means_default_even_over_dotenv() {
        let env = env_of(&[
            ("FINANCIALS_ACCESS_TOKEN", ""),
            ("FINANCIALS_API_URL", "  "),
        ]);
        let dotenv =
            "FINANCIALS_ACCESS_TOKEN=from-file\nFINANCIALS_API_URL=https://api.example.test";
        let config = Config::from_sources(env, Some(dotenv)).unwrap();
        assert_eq!(config.access_token, None);
        assert_eq!(config.api_url, DEFAULT_API_URL);
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let env = env_of(&[("FINANCIALS_ACCESS_TOKEN", "super-secret-token-123")]);
        let config = Config::from_sources(env, None).unwrap();

        let debug = format!("{config:?}");
        assert!(!debug.contains("super-secret"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        // `{:#?}` is pretty-printed Debug; it must hide the token too.
        assert!(!format!("{config:#?}").contains("super-secret"));
    }

    #[test]
    fn invalid_urls_are_rejected_with_clear_messages() {
        let cases = [
            ("localhost:8080", "must start with http:// or https://"),
            ("ftp://example.test", "must start with http:// or https://"),
            (
                "http://api.example.test",
                "must use https:// unless it points at localhost",
            ),
            ("https://", "must include a host"),
            (
                "https://user:pass@api.example.test",
                "must not contain credentials",
            ),
            ("https://api.example .test", "must not contain spaces"),
        ];
        for (url, expected) in cases {
            let env = env_of(&[("FINANCIALS_API_URL", url)]);
            let err = Config::from_sources(env, None).unwrap_err().to_string();
            assert!(err.contains(expected), "{url}: {err}");
            assert!(
                err.starts_with("configuration problem: FINANCIALS_API_URL"),
                "{err}"
            );
        }
    }

    #[test]
    fn local_http_urls_are_allowed() {
        for url in [
            "http://localhost:8080",
            "http://127.0.0.1",
            "http://[::1]:8080/",
        ] {
            let env = env_of(&[("FINANCIALS_API_URL", url)]);
            let config = Config::from_sources(env, None).unwrap();
            assert!(
                !config.api_url.ends_with('/'),
                "{url} -> {}",
                config.api_url
            );
        }
    }

    #[test]
    fn malformed_dotenv_lines_report_the_line_number_only() {
        let dotenv = "FINANCIALS_API_URL=http://localhost:8080\nsecret-token-without-a-key\n";
        let err = Config::from_sources(no_env(), Some(dotenv))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "configuration problem: .env line 2: expected KEY=VALUE"
        );
        assert!(!err.contains("secret"));

        let err = Config::from_sources(no_env(), Some("=value"))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "configuration problem: .env line 1: key is required");
    }
}
