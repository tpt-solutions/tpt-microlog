use serde::{Deserialize, Serialize};

/// Log severity levels (spec §5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl LogLevel {
    /// Numeric priority — higher is more severe.
    #[inline]
    pub fn as_u8(self) -> u8 {
        match self {
            LogLevel::Trace => 0,
            LogLevel::Debug => 1,
            LogLevel::Info => 2,
            LogLevel::Warn => 3,
            LogLevel::Error => 4,
            LogLevel::Fatal => 5,
        }
    }

    /// Severity threshold for Prometheus/OpenTelemetry-style output.
    #[inline]
    pub fn as_f64(self) -> f64 {
        f64::from(self.as_u8())
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{self:?}").to_uppercase())
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(LogLevel::Trace),
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" | "warning" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            "fatal" => Ok(LogLevel::Fatal),
            other => Err(format!("unknown log level: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Fatal > LogLevel::Error);
    }

    #[test]
    fn parsing() {
        assert_eq!("info".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!("ERROR".parse::<LogLevel>().unwrap(), LogLevel::Error);
        assert!("bogus".parse::<LogLevel>().is_err());
    }

    #[test]
    fn serialization() {
        let s = serde_json::to_string(&LogLevel::Debug).unwrap();
        assert_eq!(s, "\"DEBUG\"");
    }
}
