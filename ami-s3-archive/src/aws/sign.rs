use crate::constants::USER_AGENT;
use crate::types::Credentials;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

type HmacSha256 = Hmac<Sha256>;

pub fn sign_v4(
    method: &str,
    url: &str,
    service: &str,
    region: &str,
    creds: &Credentials,
    headers: Vec<(&str, String)>,
    body: &[u8],
) -> BTreeMap<String, String> {
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let payload_hash = hex::encode(Sha256::digest(body));
    let parsed = url::Url::parse(url).expect("valid URL");

    let mut h = BTreeMap::<String, String>::new();
    h.insert(
        "host".to_string(),
        parsed.host_str().unwrap_or_default().to_string(),
    );
    h.insert("user-agent".to_string(), USER_AGENT.to_string());
    h.insert("x-amz-date".to_string(), amz_date.clone());
    h.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
    if !creds.session_token.is_empty() {
        h.insert(
            "x-amz-security-token".to_string(),
            creds.session_token.clone(),
        );
    }
    for (k, v) in headers {
        h.insert(k.to_ascii_lowercase(), v);
    }

    let canonical_uri = if parsed.path().is_empty() {
        "/".to_string()
    } else {
        parsed.path().to_string()
    };
    let canonical_query = canonical_query_string(parsed.query().unwrap_or_default());
    let mut canonical_headers = String::new();
    for (k, v) in &h {
        canonical_headers.push_str(k);
        canonical_headers.push(':');
        canonical_headers.push_str(&normalize_header_value(v));
        canonical_headers.push('\n');
    }
    let signed_headers = h.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let signing_key = signature_key(&creds.secret_access_key, &date_stamp, region, service);
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        creds.access_key_id, credential_scope, signed_headers, signature
    );
    h.insert("authorization".to_string(), auth);
    h
}

fn signature_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn normalize_header_value(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonical_query_string(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(k, v)| (aws_encode(&k), aws_encode(&v)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn aws_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
