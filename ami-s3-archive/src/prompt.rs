use crate::error::AnyError;
use std::io::{self, Write};

pub fn ask_yn(prompt: &str, default_yes: bool) -> Result<bool, AnyError> {
    let suffix = if default_yes { " [Y/n]: " } else { " [y/N]: " };
    eprint!("{prompt}{suffix}");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let s = line.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(s.as_str(), "y" | "yes" | "o" | "oui"))
}
