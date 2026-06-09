use ami_s3_archive::{
    build_http_client, parse_args, resolve_credentials, resolve_region, run, validate_config,
    AwsClient, Logger,
};
use std::process;
use std::time::{Duration, Instant};

fn main() {
    let mut cfg = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR {e}");
            process::exit(2);
        }
    };
    let log = Logger {
        verbose: cfg.verbose,
    };

    if let Err(e) = validate_config(&mut cfg) {
        log.error(e.to_string());
        process::exit(2);
    }

    let deadline = Instant::now() + Duration::from_secs(cfg.timeout_minutes * 60);

    let region = match resolve_region(&cfg.profile, &cfg.region, log) {
        Ok(v) => v,
        Err(e) => {
            log.error(format!("region resolution failed: {e}"));
            process::exit(2);
        }
    };
    cfg.region = region.clone();

    let creds = match resolve_credentials(&cfg.profile, log) {
        Ok(v) => v,
        Err(e) => {
            log.error(format!("credential resolution failed: {e}"));
            process::exit(2);
        }
    };
    log.info(format!(
        "using AWS region={} credential_source={}",
        region, creds.source
    ));

    let http = match build_http_client() {
        Ok(c) => c,
        Err(e) => {
            log.error(format!("HTTP client initialization failed: {e}"));
            process::exit(2);
        }
    };

    let client = AwsClient::new(region, creds, http, log);
    if let Err(e) = run(&cfg, &client, deadline) {
        client.log.error(e.to_string());
        process::exit(1);
    }
}
