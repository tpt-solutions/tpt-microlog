//! PII / secret redaction, applied to entries before they reach the WAL or
//! the network.

use regex::Regex;
use serde_json::Value;

use crate::LogEntry;

/// Replacement text for redacted values.
pub const MASK: &str = "[REDACTED]";

/// Built-in detectors enabled by `redact_builtin = true`.
const BUILTIN_PATTERNS: &[(&str, &str)] = &[
    ("email", r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"),
    ("bearer", r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]+=*"),
    (
        "jwt",
        r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\b",
    ),
    ("aws-access-key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
    ("card", r"\b(?:\d[ -]?){13,19}\b"),
    ("ssn", r"\b\d{3}-\d{2}-\d{4}\b"),
];

/// Compiled redaction rules.
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    /// Lower-cased metadata keys whose values are always masked.
    keys: Vec<String>,
    patterns: Vec<Regex>,
}

impl Redactor {
    /// Build from config values. Invalid regexes are reported via
    /// [`crate::diag::report`] and skipped.
    pub fn new(keys: &[String], patterns: &[String], builtin: bool) -> Self {
        let mut compiled = Vec::new();
        if builtin {
            for (name, p) in BUILTIN_PATTERNS {
                match Regex::new(p) {
                    Ok(re) => compiled.push(re),
                    Err(e) => {
                        crate::diag::report("config", &format!("builtin redactor {name}: {e}"))
                    }
                }
            }
        }
        for p in patterns {
            match Regex::new(p) {
                Ok(re) => compiled.push(re),
                Err(e) => {
                    crate::diag::report("config", &format!("ignoring invalid redact pattern: {e}"))
                }
            }
        }
        Self {
            keys: keys.iter().map(|k| k.to_ascii_lowercase()).collect(),
            patterns: compiled,
        }
    }

    /// Whether any rule is active.
    pub fn is_enabled(&self) -> bool {
        !self.keys.is_empty() || !self.patterns.is_empty()
    }

    fn scrub_str(&self, s: &str) -> Option<String> {
        let mut out: Option<String> = None;
        for re in &self.patterns {
            let cur = out.as_deref().unwrap_or(s);
            if re.is_match(cur) {
                out = Some(re.replace_all(cur, MASK).into_owned());
            }
        }
        out
    }

    fn scrub_value(&self, v: &mut Value) {
        match v {
            Value::String(s) => {
                if let Some(new) = self.scrub_str(s) {
                    *s = new;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.scrub_value(x)),
            Value::Object(m) => self.scrub_map(m),
            _ => {}
        }
    }

    fn scrub_map(&self, m: &mut serde_json::Map<String, Value>) {
        for (k, v) in m.iter_mut() {
            if self.keys.contains(&k.to_ascii_lowercase()) {
                *v = Value::String(MASK.into());
            } else {
                self.scrub_value(v);
            }
        }
    }

    /// Redact `entry` in place (message and all metadata, recursively).
    pub fn apply(&self, entry: &mut LogEntry) {
        if let Some(new) = self.scrub_str(&entry.message) {
            entry.message = new;
        }
        self.scrub_map(&mut entry.metadata);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogLevel;

    #[test]
    fn masks_keys_patterns_and_builtins() {
        let r = Redactor::new(&["Password".into()], &[r"secret-\d+".into()], true);
        let mut e = LogEntry::new(LogLevel::Info, "mail bob@example.com used secret-42");
        e.metadata
            .insert("password".into(), Value::String("hunter2".into()));
        e.metadata.insert(
            "nested".into(),
            serde_json::json!({"auth": "Bearer abc.def", "n": 5, "list": ["ssn 123-45-6789"]}),
        );
        r.apply(&mut e);
        assert_eq!(e.message, "mail [REDACTED] used [REDACTED]");
        assert_eq!(e.metadata["password"], MASK);
        assert_eq!(e.metadata["nested"]["auth"], MASK);
        assert_eq!(e.metadata["nested"]["n"], 5);
        assert_eq!(e.metadata["nested"]["list"][0], "ssn [REDACTED]");
    }

    #[test]
    fn disabled_by_default() {
        assert!(!Redactor::new(&[], &[], false).is_enabled());
    }
}
