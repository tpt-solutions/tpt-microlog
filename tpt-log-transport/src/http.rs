//! HTTP/HTTPS transport (spec §5.3).
//!
//! Uses `reqwest` in blocking mode with the rustls TLS backend only (no
//! openssl-sys), keeping the core pipeline runtime-free: the internal worker
//! threads call `Transport::send` directly and drive their own backoff/retry.

use std::time::Duration;

use tpt_log_core::{CompressedBatch, Transport, TransportError};

/// Build a pooled blocking client with sane timeouts.
pub(crate) fn pooled_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client builder is valid")
}

/// Perform a single HTTP POST and classify the outcome. Used by every HTTP
/// backend adapter so error semantics stay consistent:
/// 408/425/429 → [`TransportError::RateLimited`] (honoring `Retry-After`),
/// other 4xx → [`TransportError::Permanent`], 5xx / network →
/// [`TransportError::Transient`].
pub(crate) fn request_once(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: &[(String, String)],
    content_type: &str,
    body: Vec<u8>,
) -> Result<(), TransportError> {
    request_once_full(client, url, headers, content_type, body).map(|_| ())
}

/// A 2xx response, for backends whose success bodies carry semantic
/// information (e.g. Elasticsearch bulk `errors: true`).
pub(crate) struct SuccessResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Like [`request_once`], but returns the body of a successful response so
/// callers can inspect it.
pub(crate) fn request_once_full(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: &[(String, String)],
    content_type: &str,
    body: Vec<u8>,
) -> Result<SuccessResponse, TransportError> {
    let mut req = client.post(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    req = req.header(reqwest::header::CONTENT_TYPE, content_type);
    let response = req.body(body).send();

    let resp = match response {
        Ok(r) => r,
        Err(e) if e.is_timeout() || e.is_connect() || e.is_request() => {
            return Err(TransportError::Transient(e.to_string()));
        }
        Err(e) => return Err(TransportError::Rejected(e.to_string())),
    };

    let status = resp.status();
    match status.as_u16() {
        200..=299 => {
            let body = resp.bytes().map(|b| b.to_vec()).unwrap_or_default();
            Ok(SuccessResponse {
                status: status.as_u16(),
                body,
            })
        }
        // 408 request timeout, 425 too early, 429 too many requests: the
        // backend is alive but asked us to slow down.
        408 | 425 | 429 => {
            let retry_after_ms = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after);
            Err(TransportError::RateLimited {
                message: format!("HTTP {status} from {url}"),
                retry_after_ms,
            })
        }
        400..=499 => Err(TransportError::Permanent(format!(
            "HTTP {status} from {url}"
        ))),
        500..=599 => Err(TransportError::Transient(format!(
            "HTTP {status} from {url}"
        ))),
        other => Err(TransportError::Rejected(format!(
            "unexpected HTTP status {other}"
        ))),
    }
}

/// Parse a `Retry-After` header's delta-seconds form. HTTP-date values (and
/// garbage) return `None` so the regular backoff applies instead.
fn parse_retry_after(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| secs.saturating_mul(1000))
}

/// How the batch is encoded in the HTTP request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyMode {
    /// POST the reconstructed JSON batch: `{"entries":[...]}`.
    Json,
    /// POST the raw compressed bytes with `Content-Encoding: zstd`.
    /// The receiver must decompress before parsing.
    Zstd,
}

/// A pooled, blocking HTTP client transport.
pub struct HttpTransport {
    client: reqwest::blocking::Client,
    url: String,
    headers: Vec<(String, String)>,
    mode: BodyMode,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport")
            .field("url", &self.url)
            .field("mode", &self.mode)
            .finish()
    }
}

impl HttpTransport {
    /// Create a transport that POSTs batches to `url`.
    pub fn new(url: impl Into<String>, headers: Vec<(String, String)>) -> Self {
        Self::with_mode(url, headers, BodyMode::Json)
    }

    /// Create a transport with an explicit body encoding mode.
    pub fn with_mode(
        url: impl Into<String>,
        headers: Vec<(String, String)>,
        mode: BodyMode,
    ) -> Self {
        HttpTransport {
            client: pooled_client(),
            url: url.into(),
            headers,
            mode,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Transport for HttpTransport {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let mut headers = self.headers.clone();
        let (content_type, body) = match self.mode {
            // Uses the JSON carried alongside the compressed bytes when
            // available: no decompress/parse/re-serialize round trip.
            BodyMode::Json => {
                let json = batch
                    .payload
                    .json_bytes()
                    .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
                ("application/json", json)
            }
            BodyMode::Zstd => {
                headers.push(("content-encoding".to_string(), "zstd".to_string()));
                ("application/json", batch.payload.bytes.clone())
            }
        };
        request_once(&self.client, &self.url, &headers, content_type, body)
    }

    fn name(&self) -> &str {
        "http"
    }
}
