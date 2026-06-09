use crate::error::AwsError;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum number of attempts for a single S3 request (initial try + retries).
pub const S3_MAX_ATTEMPTS: u32 = 10;

/// Base delay for exponential backoff (milliseconds).
const S3_BACKOFF_BASE_MS: u64 = 100;

/// Cap on exponential backoff delay before jitter (milliseconds).
const S3_BACKOFF_CAP_MS: u64 = 20_000;

/// Returns true when the error is transient and the request should be retried.
pub fn is_s3_retryable(err: &AwsError) -> bool {
    if err.status == 0 {
        return true;
    }
    if err.status == 429 {
        return true;
    }
    if (500..=504).contains(&err.status) {
        return true;
    }
    err.body.contains("RequestTimeout")
        || err.body.contains("SlowDown")
        || err.body.contains("ServiceUnavailable")
        || err.body.contains("InternalError")
}

/// Full jitter backoff per AWS guidance:
/// <https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/>
///
/// `sleep = random_between(0, min(cap, base * 2^attempt))`
pub fn s3_backoff_delay(attempt: u32) -> Duration {
    let exp = S3_BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(31));
    let ceiling = exp.min(S3_BACKOFF_CAP_MS);
    Duration::from_millis(jitter_ms(ceiling))
}

fn jitter_ms(max_ms: u64) -> u64 {
    if max_ms == 0 {
        return 0;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    nanos % (max_ms + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_status_codes() {
        assert!(is_s3_retryable(&AwsError {
            service: "s3".into(),
            method: "PUT".into(),
            status: 0,
            body: "connection reset".into(),
        }));
        assert!(is_s3_retryable(&AwsError {
            service: "s3".into(),
            method: "PUT".into(),
            status: 503,
            body: "SlowDown".into(),
        }));
        assert!(!is_s3_retryable(&AwsError {
            service: "s3".into(),
            method: "HEAD".into(),
            status: 404,
            body: "Not Found".into(),
        }));
        assert!(!is_s3_retryable(&AwsError {
            service: "s3".into(),
            method: "PUT".into(),
            status: 403,
            body: "AccessDenied".into(),
        }));
    }

    #[test]
    fn backoff_stays_within_cap() {
        for attempt in 0..20 {
            let delay = s3_backoff_delay(attempt);
            assert!(delay <= Duration::from_millis(S3_BACKOFF_CAP_MS));
        }
    }
}
