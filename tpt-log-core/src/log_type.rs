use serde::{Deserialize, Serialize};

/// Classification of a log entry (spec §5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogType {
    /// Human/structured log message.
    Log,
    /// Numeric measurement (gauge, counter, histogram).
    Metric,
    /// Distributed tracing span data (spec §5.1, Phase 8).
    Trace,
}

impl std::fmt::Display for LogType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        assert_eq!(
            serde_json::to_string(&LogType::Metric).unwrap(),
            "\"metric\""
        );
        assert_eq!(serde_json::to_string(&LogType::Trace).unwrap(), "\"trace\"");
    }
}
