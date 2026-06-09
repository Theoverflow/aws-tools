use chrono::Utc;

#[derive(Debug, Clone, Copy)]
pub struct Logger {
    pub verbose: bool,
}

impl Logger {
    pub fn info(&self, msg: impl AsRef<str>) {
        println!("{} INFO  {}", Utc::now().to_rfc3339(), msg.as_ref());
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        eprintln!("{} WARN  {}", Utc::now().to_rfc3339(), msg.as_ref());
    }

    pub fn error(&self, msg: impl AsRef<str>) {
        eprintln!("{} ERROR {}", Utc::now().to_rfc3339(), msg.as_ref());
    }

    pub fn debug(&self, msg: impl AsRef<str>) {
        if self.verbose {
            eprintln!("{} DEBUG {}", Utc::now().to_rfc3339(), msg.as_ref());
        }
    }
}
