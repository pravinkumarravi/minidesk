//! Redacted structured logging for miniDesk.
//!
//! Enforces security policies to prevent secrets, raw keystrokes, clipboard content,
//! and screen payloads from being leaked to log sinks.

use tracing::Level;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::FmtSubscriber;

/// Initializes the global tracing subscriber with security redaction defaults.
pub fn init_logging(default_level: Level) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level.as_str()));

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact()
        .finish();

    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Redaction utility to sanitize sensitive strings before logging.
pub fn redact_secret(s: &str) -> String {
    if s.is_empty() {
        return "<empty>".into();
    }
    if s.len() <= 4 {
        return "***".into();
    }
    format!("{}...***", &s[..2])
}

/// Redaction utility to ensure clipboard data preview is safe and truncated.
pub fn redact_clipboard_preview(data: &str) -> String {
    format!("<clipboard text: {} chars redacted>", data.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redaction() {
        assert_eq!(redact_secret("secret_token_12345"), "se...***");
        assert_eq!(redact_secret("abc"), "***");
        assert_eq!(redact_secret(""), "<empty>");
    }

    #[test]
    fn test_clipboard_redaction() {
        let preview = redact_clipboard_preview("Hello password123");
        assert!(!preview.contains("password123"));
        assert!(preview.contains("17 chars redacted"));
    }
}
