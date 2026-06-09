use std::error::Error;
use std::fmt;

pub type AnyError = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub struct AwsError {
    pub service: String,
    pub method: String,
    pub status: u16,
    pub body: String,
}

impl fmt::Display for AwsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AWS {} {} failed: HTTP {}: {}",
            self.service,
            self.method,
            self.status,
            summarize(&self.body)
        )
    }
}

impl Error for AwsError {}

pub fn summarize(s: &str) -> String {
    let t = s.trim();
    if t.len() > 2000 {
        format!("{}...", &t[..2000])
    } else {
        t.to_string()
    }
}

pub fn empty_default<'a>(s: &'a str, d: &'a str) -> &'a str {
    if s.is_empty() { d } else { s }
}
