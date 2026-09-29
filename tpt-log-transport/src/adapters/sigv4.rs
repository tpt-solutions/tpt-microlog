//! Minimal AWS Signature V4 signer (used by the CloudWatch adapter).
//!
//! Hand-rolled so the transport layer avoids `rusoto`/`aws-sdk-sigv4`
//! heavyweight dependencies. Balances correctness with the microscopic
//! mandate: JSON payloads, path + header signing, Windows-agnostic canonical
//! requests, and a UTC clock derived from `SystemTime`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Split a year into (year, month, day) from a count of days since 1970-01-01
/// (Howard Hinnant's civil_from_days).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m as u32,
        d as u32,
    )
}

/// UTC `YYYYMMDD`/`YYYYMMDD'T'HHMMSS'Z'` from the system clock.
fn utc_now() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    let date = format!("{y:04}{mo:02}{d:02}");
    let stamp = format!("{date}T{h:02}{m:02}{s:02}Z");
    (date, stamp)
}

/// AWS SigV4 credentials + time window for signing.
#[derive(Clone, Debug)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
}

/// Sign an AWS API request and return the header values to attach:
/// `("Authorization", "x-amz-date")`.
pub fn sign(
    creds: &Credentials,
    service: &str,
    method: &str,
    host: &str,
    path: &str,
    extra_headers: &BTreeMap<String, String>,
    payload: &[u8],
) -> (String, String) {
    let (date_stamp, amz_date) = utc_now();

    // Canonical headers: host, extra ones (content-type, x-amz-target, etc.),
    // and x-amz-date — all lowercased and sorted by name.
    let mut canonical: BTreeMap<String, String> = BTreeMap::new();
    canonical.insert("host".to_string(), host.to_string());
    canonical.insert("x-amz-date".to_string(), amz_date.clone());
    for (k, v) in extra_headers {
        canonical.insert(k.to_ascii_lowercase(), v.to_string());
    }
    let canonical_headers: String = canonical
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect();
    let signed_headers: Vec<&str> = canonical.keys().map(String::as_str).collect();
    let signed_headers_str = signed_headers.join(";");

    let payload_hash = sha256_hex(payload);
    let canonical_request =
        format!("{method}\n{path}\n\n{canonical_headers}{signed_headers_str}\n{payload_hash}");

    let scope = format!("{date_stamp}/{}/{service}/aws4_request", creds.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let mut key = hmac_sha256(
        format!("AWS4{}", creds.secret_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    key = hmac_sha256(&key, creds.region.as_bytes());
    key = hmac_sha256(&key, service.as_bytes());
    key = hmac_sha256(&key, b"aws4_request");
    let signature = hex(&hmac_sha256(&key, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers_str}, Signature={signature}",
        creds.access_key
    );
    (authorization, amz_date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_key_chain_is_reproducible_and_signature_is_64_hex_chars() {
        // The signature derives from a deterministic key chain; re-deriving the
        // same inputs must produce an identical 64-char lowercase hex digest.
        let key = b"\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f";
        let data = b"example";
        let a = hmac_sha256(key, data);
        let b = hmac_sha256(key, b"example");
        assert_eq!(a, b);
        assert_eq!(hex(&a).len(), 64);
        assert!(hex(&a)
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn signer_emits_expected_header_shape() {
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
        };
        let mut headers = BTreeMap::new();
        headers.insert("content-type".into(), "application/x-amz-json-1.1".into());
        headers.insert("x-amz-target".into(), "Logs_20140328.PutLogEvents".into());
        let (auth, amz_date) = sign(
            &creds,
            "logs",
            "POST",
            "logs.us-east-1.amazonaws.com",
            "/",
            &headers,
            b"{}",
        );
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(auth.contains("/us-east-1/logs/aws4_request"));
        assert!(auth.contains("Signature="));
        assert!(auth.contains("x-amz-date"));
        assert_eq!(amz_date.len(), 16);
        assert!(amz_date.ends_with('Z'));
    }
}
