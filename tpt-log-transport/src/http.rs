//! HTTP/HTTPS transport (spec §5.3).
//!
//! Uses `reqwest` in blocking mode with the rustls TLS backend only (no
//! openssl-sys), keeping the core pipeline runtime-free: the internal worker
//! threads call `Transport::send` directly and drive their own backoff/retry.

use std::time::Duration;

use tpt_log_core::{Batch, CompressedBatch, Transport, TransportError};

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
/// 4xx → [`TransportError::Permanent`], 5xx / network → [`TransportError::Transient`].
pub(crate) fn request_once(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: &[(String, String)],
    content_type: &str,
    body: Vec<u8>,
) -> Result<(), TransportError> {
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
        200..=299 => Ok(()),
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
            BodyMode::Json => {
                let entries = batch
                    .decompress_entries()
                    .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
                ("application/json", Batch { entries }.to_json())
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
