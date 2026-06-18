//! Secret redaction for tool output. Scrubs secret-shaped strings (by key-name
//! pattern and by known env value) before tool output re-enters the model
//! context or the session trajectory.

use serde_json::Value;

/// Suffixes (case-insensitive) that mark a key as secret-shaped.
const SECRET_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_CREDENTIAL"];

/// Minimum length for a value to be eligible for the value-scan (avoids
/// corrupting output by matching short/common substrings).
const MIN_SECRET_LEN: usize = 8;

const REDACTED: &str = "***";

/// A snapshot of secret values eligible for literal value-scanning. Built once
/// per run; values below 8 chars (see `MIN_SECRET_LEN`) or that look like common
/// words are dropped.
#[derive(Debug, Clone, Default)]
pub struct SecretSet {
    values: Vec<String>,
}

impl SecretSet {
    /// Build a set from explicit values, applying the length/entropy floor.
    pub fn from_values(values: Vec<String>) -> Self {
        let values = values
            .into_iter()
            .filter(|v| v.len() >= MIN_SECRET_LEN && !is_common_word(v))
            .collect();
        Self { values }
    }

    /// Snapshot the parent process environment: take the values of variables
    /// whose names match a secret suffix, then `extra`, applying the floor.
    pub fn from_env_and_extra(extra: &[String]) -> Self {
        let mut values: Vec<String> = std::env::vars()
            .filter(|(name, _)| name_is_secret(name))
            .map(|(_, val)| val)
            .collect();
        values.extend(extra.iter().cloned());
        Self::from_values(values)
    }
}

fn name_is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
}

fn is_common_word(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "true" | "false" | "dev" | "prod" | "test" | "none" | "null"
    ) || v.chars().all(|c| c.is_ascii_digit())
}

/// Walk `value`, redacting every string within. Never errors.
///
/// # Limitations (v1)
/// - **One assignment per line:** only the first `KEY=`/`KEY:` on a line is
///   checked, so a secret in a second assignment on the same line
///   (`FOO=1 BAR_TOKEN=secret`) is matched only if its value is in the env-sourced
///   value set. One-variable-per-line output (the common `env`/`.env` shape) is
///   fully covered.
/// - **Value-scan is case-sensitive:** an env-sourced secret that appears with
///   different casing in output is not caught by the value-scan.
/// - **Key-name scan is underscore-form only** (`X_API_KEY`, not `X-API-KEY`)
///   and does not scan JSON object keys.
pub fn redact(value: &Value, secrets: &SecretSet) -> Value {
    match value {
        Value::String(s) => Value::String(redact_str(s, secrets)),
        Value::Array(a) => Value::Array(a.iter().map(|v| redact(v, secrets)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), redact(v, secrets)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn redact_str(s: &str, secrets: &SecretSet) -> String {
    let mut out = redact_key_values(s);
    for secret in &secrets.values {
        if out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

/// Replace the value in `KEY=val` / `KEY: val` / `export KEY=val` where KEY ends
/// in a secret suffix. Operates line-by-line.
fn redact_key_values(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        result.push_str(&redact_line(line));
    }
    result
}

fn redact_line(line: &str) -> String {
    for sep in ['=', ':'] {
        if let Some(pos) = line.find(sep) {
            let (head, tail) = line.split_at(pos);
            let key = head.rsplit([' ', '\t']).next().unwrap_or(head).trim();
            if name_is_secret(key) {
                let after = &tail[1..]; // skip the separator
                let (space, rest) = match after.strip_prefix(' ') {
                    Some(r) => (" ", r),
                    None => ("", after),
                };
                if rest.trim().is_empty() {
                    return line.to_string(); // KEY= with no value: leave as-is
                }
                let trailing_ws: String = rest
                    .chars()
                    .rev()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                let trailing: String = trailing_ws.chars().rev().collect();
                return format!("{head}{sep}{space}{REDACTED}{trailing}");
            }
        }
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(values: &[&str]) -> SecretSet {
        SecretSet::from_values(values.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn redacts_key_name_patterns() {
        let out = redact(
            &json!({ "stdout": "OPENAI_API_KEY=sk-abc123xyz\n" }),
            &set(&[]),
        );
        assert_eq!(out["stdout"], "OPENAI_API_KEY=***\n");
        let out = redact(
            &json!({ "stdout": "export DB_PASSWORD=hunter2pass" }),
            &set(&[]),
        );
        assert_eq!(out["stdout"], "export DB_PASSWORD=***");
        let out = redact(&json!({ "stdout": "AUTH_TOKEN: abcdefgh12" }), &set(&[]));
        assert_eq!(out["stdout"], "AUTH_TOKEN: ***");
    }

    #[test]
    fn redacts_known_env_values() {
        let out = redact(
            &json!({ "stdout": "using sk-abc123xyz to auth" }),
            &set(&["sk-abc123xyz"]),
        );
        assert_eq!(out["stdout"], "using *** to auth");
    }

    #[test]
    fn value_scan_has_a_length_floor() {
        let s = SecretSet::from_values(vec!["dev".into(), "true".into(), "1234".into()]);
        let out = redact(&json!({ "stdout": "mode=dev ok=true n=1234" }), &s);
        assert_eq!(out["stdout"], "mode=dev ok=true n=1234");
    }

    #[test]
    fn walks_nested_json_and_is_idempotent() {
        let v = json!({ "a": { "b": ["X_TOKEN=abcdefgh12"] } });
        let once = redact(&v, &set(&[]));
        assert_eq!(once["a"]["b"][0], "X_TOKEN=***");
        assert_eq!(redact(&once, &set(&[])), once);
    }
}
