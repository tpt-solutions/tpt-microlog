//! CloudWatch Logs adapter (spec §5.3).
//!
//! POSTs `PutLogEvents` to CloudWatch Logs with AWS SigV4 signing. Requires
//! region + access key + secret key + log group/stream from the core config.

use std::collections::BTreeMap;

use tpt_log_core::CompressedBatch;
use tpt_log_core::{Config, LogEntry, Transport, TransportError};

use crate::adapters::sigv4::{self, Credentials};
use crate::http::{pooled_client, request_once};

const SERVICE: &str = "logs";

/// CloudWatch Logs PutLogEvents adapter.
pub struct CloudWatchAdapter {
    client: reqwest::blocking::Client,
    url: String,
    host: String,
    creds: Credentials,
    log_group: String,
    log_stream: String,
}

impl std::fmt::Debug for CloudWatchAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudWatchAdapter")
            .field("url", &self.url)
            .field("log_group", &self.log_group)
            .field("log_stream", &self.log_stream)
            .finish_non_exhaustive()
    }
}

impl CloudWatchAdapter {
    /// Build from core config. Requires `aws_region`, `aws_access_key`,
    /// `aws_secret_key`, `aws_log_group`, `aws_log_stream`. The endpoint
    /// defaults to `https://logs.<region>.amazonaws.com`.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let region = config.aws_region.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_region`".into())
        })?;
        let access_key = config.aws_access_key.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_access_key`".into())
        })?;
        let secret_key = config.aws_secret_key.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_secret_key`".into())
        })?;
        let log_group = config.aws_log_group.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_log_group`".into())
        })?;
        let log_stream = config.aws_log_stream.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_log_stream`".into())
        })?;

        let host = config
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("logs.{region}.amazonaws.com"));
        let url = if host.starts_with("http://") || host.starts_with("https://") {
            host.clone()
        } else {
            format!("https://{host}")
        };
        let hostname = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or(&host)
            .to_string();

        Ok(Self {
            client: pooled_client(),
            url,
            host: hostname.to_string(),
            creds: Credentials {
                access_key,
                secret_key,
                region,
            },
            log_group,
            log_stream,
        })
    }

    fn build_body(&self, entries: &[LogEntry]) -> serde_json::Value {
        let events: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "timestamp": e.timestamp,
                    "message": e.message,
                })
            })
            .collect();
        serde_json::json!({
            "logGroupName": self.log_group,
            "logStreamName": self.log_stream,
            "logEvents": events,
        })
    }
}

impl Transport for CloudWatchAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        let body = serde_json::to_vec(&self.build_body(&entries))
            .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;

        let mut extra = BTreeMap::new();
        extra.insert("content-type".into(), "application/x-amz-json-1.1".into());
        extra.insert("x-amz-target".into(), "Logs_20140328.PutLogEvents".into());
        let (auth, amz_date) =
            sigv4::sign(&self.creds, SERVICE, "POST", &self.host, "/", &extra, &body);

        let headers = vec![
            ("authorization".to_string(), auth),
            ("x-amz-date".to_string(), amz_date),
            (
                "content-type".to_string(),
                "application/x-amz-json-1.1".to_string(),
            ),
            (
                "x-amz-target".to_string(),
                "Logs_20140328.PutLogEvents".to_string(),
            ),
        ];
        request_once(
            &self.client,
            &self.url,
            &headers,
            "application/x-amz-json-1.1",
            body,
        )
    }

    fn name(&self) -> &str {
        "cloudwatch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::LogLevel;

    #[test]
    fn build_body_shape() {
        let cfg = Config {
            aws_region: Some("eu-west-1".into()),
            aws_access_key: Some("AKID".into()),
            aws_secret_key: Some("SECRET".into()),
            aws_log_group: Some("group".into()),
            aws_log_stream: Some("stream".into()),
            endpoint: Some("https://logs.eu-west-1.amazonaws.com".into()),
            ..Config::default()
        };
        let adapter = CloudWatchAdapter::from_config(&cfg).unwrap();
        let entries = vec![LogEntry {
            timestamp: 42,
            level: LogLevel::Info,
            log_type: tpt_log_core::LogType::Log,
            message: "hi".into(),
            metadata: Default::default(),
        }];
        let body = adapter.build_body(&entries);
        assert_eq!(body["logGroupName"], "group");
        assert_eq!(body["logStreamName"], "stream");
        assert_eq!(body["logEvents"][0]["timestamp"], 42);
        assert_eq!(body["logEvents"][0]["message"], "hi");
    }

    #[test]
    fn requires_all_aws_fields() {
        let cfg = Config {
            aws_region: Some("us-east-1".into()),
            ..Config::default()
        };
        assert!(CloudWatchAdapter::from_config(&cfg).is_err());
    }
}
