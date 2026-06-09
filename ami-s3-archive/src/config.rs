use crate::error::AnyError;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub ami_id: String,
    pub region: String,
    pub profile: String,
    pub s3_uri: String,
    pub bucket: String,
    pub create_bucket: bool,
    pub cleanup: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub wait: bool,
    pub poll_seconds: u64,
    pub timeout_minutes: u64,
    pub storage_class: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ami_id: String::new(),
            region: String::new(),
            profile: String::new(),
            s3_uri: String::new(),
            bucket: String::new(),
            create_bucket: false,
            cleanup: false,
            yes: false,
            dry_run: false,
            verbose: false,
            wait: true,
            poll_seconds: 15,
            timeout_minutes: 720,
            storage_class: "STANDARD_IA".to_string(),
        }
    }
}

pub fn parse_args() -> Result<Config, AnyError> {
    let mut cfg = Config::default();
    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--ami-id" => cfg.ami_id = take_value(&mut args, "--ami-id")?,
            "--region" => cfg.region = take_value(&mut args, "--region")?,
            "--profile" => cfg.profile = take_value(&mut args, "--profile")?,
            "--s3-uri" => cfg.s3_uri = take_value(&mut args, "--s3-uri")?,
            "--storage-class" => cfg.storage_class = take_value(&mut args, "--storage-class")?,
            "--poll-seconds" => {
                cfg.poll_seconds = take_value(&mut args, "--poll-seconds")?.parse()?
            }
            "--timeout-minutes" => {
                cfg.timeout_minutes = take_value(&mut args, "--timeout-minutes")?.parse()?
            }
            "--create-bucket" => cfg.create_bucket = true,
            "--cleanup" => cfg.cleanup = true,
            "--yes" | "-y" => cfg.yes = true,
            "--dry-run" => cfg.dry_run = true,
            "--verbose" | "-v" => cfg.verbose = true,
            "--wait" => cfg.wait = true,
            "--no-wait" => cfg.wait = false,
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    Ok(cfg)
}

fn take_value<I>(args: &mut std::iter::Peekable<I>, name: &str) -> Result<String, AnyError>
where
    I: Iterator<Item = String>,
{
    match args.next() {
        Some(v) if !v.starts_with("--") => Ok(v),
        _ => Err(format!("{name} requires a value").into()),
    }
}

pub fn print_help() {
    println!(
        r#"ami-s3-archive

Archive an EC2 AMI to one S3 .bin object with EC2 CreateStoreImageTask, then rewrite the object to the requested S3 storage class, typically STANDARD_IA.

USAGE:
  ami-s3-archive --ami-id ami-0123456789abcdef0 --region eu-west-3 --s3-uri s3://bucket [options]

OPTIONS:
  --ami-id ID             Required AMI ID.
  --region REGION         AWS region. Auto-detected from env/profile/IMDS when omitted.
  --profile PROFILE       AWS profile. Defaults to AWS_PROFILE or default.
  --s3-uri s3://BUCKET    Target bucket root. Object key is usually ami-id.bin.
  --create-bucket         Create deterministic/specified bucket if missing.
  --storage-class CLASS   Final object class. Default STANDARD_IA.
  --cleanup               After archive, ask to deregister AMI and delete associated snapshots.
  --yes, -y               Non-interactive yes for bucket creation and cleanup.
  --dry-run               Check permissions/plan without mutating resources where possible.
  --wait / --no-wait      Wait for store task completion. Default --wait.
  --poll-seconds N        Poll interval. Minimum 5 seconds. Default 15.
  --timeout-minutes N     Overall timeout guard. Default 720.
  --verbose, -v           Debug logs to stderr.
  --help, -h              Show this help.

CREDENTIAL SOURCES:
  env AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY/AWS_SESSION_TOKEN,
  ~/.aws/credentials profile,
  ECS container credentials,
  EC2 IMDS role credentials.
"#
    );
}

pub fn validate_config(cfg: &mut Config) -> Result<(), AnyError> {
    if cfg.ami_id.is_empty() {
        return Err("--ami-id is required".into());
    }
    if !is_valid_ami_id(&cfg.ami_id) {
        return Err(format!("invalid --ami-id: {}", cfg.ami_id).into());
    }
    if cfg.profile.is_empty() {
        cfg.profile = env::var("AWS_PROFILE").unwrap_or_default();
    }
    if cfg.profile.is_empty() {
        cfg.profile = "default".to_string();
    }
    cfg.poll_seconds = cfg.poll_seconds.max(5);
    cfg.timeout_minutes = cfg.timeout_minutes.max(1);
    if cfg.storage_class.is_empty() {
        cfg.storage_class = "STANDARD_IA".to_string();
    }
    if !cfg.s3_uri.is_empty() {
        cfg.bucket = parse_s3_bucket(&cfg.s3_uri)?;
    }
    Ok(())
}

pub fn is_valid_ami_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("ami-") else {
        return false;
    };
    (8..=17).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn parse_s3_bucket(s: &str) -> Result<String, AnyError> {
    let Some(rest) = s.strip_prefix("s3://") else {
        return Err(format!("invalid --s3-uri: expected s3://bucket, got {s}").into());
    };
    let mut parts = rest.split('/');
    let bucket = parts.next().unwrap_or_default();
    if bucket.is_empty() || parts.any(|p| !p.is_empty()) {
        return Err(format!("--s3-uri must point to a bucket root only, got {s}").into());
    }
    Ok(bucket.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ami_id() {
        assert!(is_valid_ami_id("ami-0123456789abcdef0"));
        assert!(!is_valid_ami_id("snap-abc"));
    }

    #[test]
    fn parses_bucket_root_only() {
        assert_eq!(parse_s3_bucket("s3://my-bucket").unwrap(), "my-bucket");
        assert!(parse_s3_bucket("s3://my-bucket/key").is_err());
    }
}
