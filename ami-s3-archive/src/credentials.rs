use crate::error::AnyError;
use crate::log::Logger;
use crate::types::Credentials;
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

static CREDENTIALS_INI: OnceLock<HashMap<String, HashMap<String, String>>> = OnceLock::new();
static CONFIG_INI: OnceLock<HashMap<String, HashMap<String, String>>> = OnceLock::new();

pub fn resolve_region(profile: &str, explicit: &str, log: Logger) -> Result<String, AnyError> {
    if !explicit.is_empty() {
        return Ok(explicit.to_string());
    }
    for key in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
        if let Ok(v) = env::var(key) {
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    if let Some(v) = profile_value(profile, "region") {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Ok(v) = imds_region(&Client::builder().timeout(Duration::from_secs(2)).build()?, log) {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    Err("region not found; pass --region or set AWS_REGION/AWS_DEFAULT_REGION/profile region".into())
}

pub fn resolve_credentials(profile: &str, _log: Logger) -> Result<Credentials, AnyError> {
    if let Ok(ak) = env::var("AWS_ACCESS_KEY_ID") {
        if !ak.is_empty() {
            let sk = env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default();
            if sk.is_empty() {
                return Err("AWS_ACCESS_KEY_ID is set but AWS_SECRET_ACCESS_KEY is empty".into());
            }
            return Ok(Credentials {
                access_key_id: ak,
                secret_access_key: sk,
                session_token: env::var("AWS_SESSION_TOKEN").unwrap_or_default(),
                source: "env".to_string(),
            });
        }
    }
    if let Some(c) = profile_creds(profile) {
        return Ok(c);
    }
    let http = Client::builder().timeout(Duration::from_secs(5)).build()?;
    if let Ok(c) = ecs_creds(&http) {
        if !c.access_key_id.is_empty() {
            return Ok(c);
        }
    }
    if let Ok(c) = imds_creds(&http, _log) {
        if !c.access_key_id.is_empty() {
            return Ok(c);
        }
    }
    Err(format!("no credentials found in env, profile {profile:?}, ECS, or EC2 IMDS").into())
}

fn profile_creds(profile: &str) -> Option<Credentials> {
    let ini = credentials_ini();
    let section = ini.get(profile)?;
    let ak = section.get("aws_access_key_id")?.to_string();
    let sk = section.get("aws_secret_access_key")?.to_string();
    if ak.is_empty() || sk.is_empty() {
        return None;
    }
    Some(Credentials {
        access_key_id: ak,
        secret_access_key: sk,
        session_token: section.get("aws_session_token").cloned().unwrap_or_default(),
        source: format!("profile:{profile}"),
    })
}

fn profile_value(profile: &str, key: &str) -> Option<String> {
    let ini = config_ini();
    let section_name = if profile == "default" {
        "default".to_string()
    } else {
        format!("profile {profile}")
    };
    ini.get(&section_name)
        .and_then(|s| s.get(key))
        .cloned()
        .or_else(|| ini.get(profile).and_then(|s| s.get(key)).cloned())
}

fn credentials_ini() -> &'static HashMap<String, HashMap<String, String>> {
    CREDENTIALS_INI.get_or_init(|| {
        let path = env::var("AWS_SHARED_CREDENTIALS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".aws").join("credentials"));
        parse_ini_file(&path)
    })
}

fn config_ini() -> &'static HashMap<String, HashMap<String, String>> {
    CONFIG_INI.get_or_init(|| {
        let path = env::var("AWS_CONFIG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".aws").join("config"));
        parse_ini_file(&path)
    })
}

fn parse_ini_file(path: &Path) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let Ok(data) = fs::read_to_string(path) else {
        return out;
    };
    let mut current = String::new();
    for raw in data.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.contains(']') {
            current = line[1..line.find(']').unwrap_or(1)].trim().to_string();
            out.entry(current.clone()).or_default();
            continue;
        }
        if current.is_empty() {
            continue;
        }
        if let Some(i) = line.find('=') {
            let k = line[..i].trim().to_ascii_lowercase();
            let v = line[i + 1..]
                .trim()
                .trim_matches(&['"', '\''][..])
                .to_string();
            out.entry(current.clone()).or_default().insert(k, v);
        }
    }
    out
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn imds_token(http: &Client) -> String {
    match http
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "21600")
        .send()
    {
        Ok(r) if r.status().is_success() => r.text().unwrap_or_default(),
        _ => String::new(),
    }
}

fn imds_get(http: &Client, path: &str, log: Logger) -> Result<String, AnyError> {
    let token = imds_token(http);
    let url = format!("http://169.254.169.254/latest/{}", path.trim_start_matches('/'));
    let mut req = http.get(url);
    if !token.is_empty() {
        req = req.header("X-aws-ec2-metadata-token", token);
    }
    log.debug(format!("IMDS GET {path}"));
    let resp = req.send()?;
    if !resp.status().is_success() {
        return Err(format!("IMDS status {}", resp.status()).into());
    }
    Ok(resp.text()?)
}

fn imds_region(http: &Client, log: Logger) -> Result<String, AnyError> {
    let body = imds_get(http, "dynamic/instance-identity/document", log)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(v.get("region")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string())
}

fn imds_creds(http: &Client, log: Logger) -> Result<Credentials, AnyError> {
    let roles = imds_get(http, "meta-data/iam/security-credentials/", log)?;
    let role = roles.lines().next().unwrap_or_default().trim();
    if role.is_empty() {
        return Err("no IMDS role".into());
    }
    let body = imds_get(http, &format!("meta-data/iam/security-credentials/{role}"), log)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(Credentials {
        access_key_id: v
            .get("AccessKeyId")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        secret_access_key: v
            .get("SecretAccessKey")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        session_token: v
            .get("Token")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        source: format!("ec2-imds:{role}"),
    })
}

fn ecs_creds(http: &Client) -> Result<Credentials, AnyError> {
    let uri = env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").unwrap_or_else(|_| {
        env::var("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .map(|rel| format!("http://169.254.170.2{rel}"))
            .unwrap_or_default()
    });
    if uri.is_empty() {
        return Err("no ECS credentials URI".into());
    }
    let mut req = http.get(uri);
    if let Ok(tok) = env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN") {
        if !tok.is_empty() {
            req = req.header("Authorization", tok);
        }
    }
    let resp = req.send()?;
    if !resp.status().is_success() {
        return Err(format!("ECS credentials status {}", resp.status()).into());
    }
    let v: serde_json::Value = serde_json::from_str(&resp.text()?)?;
    Ok(Credentials {
        access_key_id: v
            .get("AccessKeyId")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        secret_access_key: v
            .get("SecretAccessKey")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        session_token: v
            .get("Token")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        source: "ecs-container".to_string(),
    })
}
