//! AI inference, Function Calling, and media generation client configuration for GFCut Studio.
//!
//! Enforces realistic 90-second HTTP timeouts for complex video/image/agent calls and
//! automatic exponential backoff (2s, then 4s) when Google AI Studio returns HTTP 503 (`UNAVAILABLE`)
//! or HTTP 429 (`RESOURCE_EXHAUSTED`).

use std::time::Duration;

pub use crate::ffmpeg::{
    AiBrollModel, AiBrollOutput, BrollAttemptError, attempt_gemini_storyboard,
    discover_available_gemini_models, gemini_generate_with_tools, generate_ai_broll,
    generate_ai_broll_with_model, generate_ai_broll_with_model_and_res, select_gemini_chat_model,
};

/// Realistic HTTP timeout (90 seconds) so complex Gemini tool-calling, storyboard,
/// and Imagen 3 video/frame generation requests complete without premature socket aborts.
pub const AI_HTTP_TIMEOUT_SECS: u32 = 90;

/// Exponential backoff schedule (in seconds) for HTTP 503 (`Service Unavailable` / `UNAVAILABLE`)
/// and HTTP 429 retries: 2 retries waiting 2s and 4s before reporting a visible error.
pub const AI_RETRY_BACKOFF_SECS: [u64; 2] = [2, 4];

/// Returns the backoff [`Duration`]s used when retrying transient 503/429 responses.
pub fn retry_backoff_durations() -> [Duration; 2] {
    [
        Duration::from_secs(AI_RETRY_BACKOFF_SECS[0]),
        Duration::from_secs(AI_RETRY_BACKOFF_SECS[1]),
    ]
}

/// Returns `true` when the HTTP status code or response body indicates a transient
/// `503 Service Unavailable` (`UNAVAILABLE`) or `429 Too Many Requests` condition
/// that should be retried with exponential backoff.
pub fn is_retryable_http_status(http_code: u16, body: &str) -> bool {
    http_code == 503
        || http_code == 429
        || body.contains("\"UNAVAILABLE\"")
        || body.contains("Service Unavailable")
        || body.contains("HTTP 503")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_and_exponential_backoff_constants_match_spec() {
        assert_eq!(AI_HTTP_TIMEOUT_SECS, 90);
        assert_eq!(AI_RETRY_BACKOFF_SECS, [2, 4]);
        let durations = retry_backoff_durations();
        assert_eq!(durations[0], Duration::from_secs(2));
        assert_eq!(durations[1], Duration::from_secs(4));
    }

    #[test]
    fn detects_http_503_unavailable_as_retryable() {
        assert!(is_retryable_http_status(503, ""));
        assert!(is_retryable_http_status(
            200,
            r#"{"error":{"code":503,"status":"UNAVAILABLE"}}"#
        ));
        assert!(!is_retryable_http_status(400, r#"{"error":{"code":400}}"#));
        assert!(!is_retryable_http_status(200, r#"{"candidates":[]}"#));
    }
}
