use regex::Regex;
use serde_json::Value;

use super::{LocalFailoverPolicy, LocalFailoverRegexRule};

const SESSION_PRESERVING_OVERLOAD_MESSAGE: &str =
    "Our servers are currently overloaded. Please try again later.";
const SESSION_PRESERVING_RATE_LIMIT_MESSAGE: &str = "Rate limit exceeded";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ParsedLocalErrorResponse {
    message: Option<String>,
    reason: Option<String>,
    raw: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalFailoverInput<'a> {
    pub(crate) status_code: u16,
    pub(crate) response_text: Option<&'a str>,
}

impl<'a> LocalFailoverInput<'a> {
    pub(crate) fn new(status_code: u16, response_text: Option<&'a str>) -> Self {
        Self {
            status_code,
            response_text: response_text
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalFailoverClassification {
    UseDefault,
    StopStatusCode,
    StopErrorPattern,
    StopExecutionError,
    RetrySuccessPattern,
    RetryStatusCode,
    RetryUpstreamFailure,
}

impl LocalFailoverClassification {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UseDefault => "use_default",
            Self::StopStatusCode => "stop_status_code",
            Self::StopErrorPattern => "stop_error_pattern",
            Self::StopExecutionError => "stop_execution_error",
            Self::RetrySuccessPattern => "retry_success_pattern",
            Self::RetryStatusCode => "retry_status_code",
            Self::RetryUpstreamFailure => "retry_upstream_failure",
        }
    }
}

pub(crate) fn classify_local_failover(
    policy: &LocalFailoverPolicy,
    input: LocalFailoverInput<'_>,
) -> LocalFailoverClassification {
    // This capacity error is transient and account-independent. Retrying another
    // candidate would move a sticky session to a different account, so it must
    // take precedence over provider-configured retryable status codes.
    if is_session_preserving_overload(input) {
        return LocalFailoverClassification::StopErrorPattern;
    }

    // A generic gateway/proxy 429 is account-independent, just like the
    // session-preserving overload above. Do not replace the current candidate.
    if is_session_preserving_rate_limit(input.status_code, input.response_text) {
        return LocalFailoverClassification::StopErrorPattern;
    }

    if policy.stop_status_codes.contains(&input.status_code) {
        return LocalFailoverClassification::StopStatusCode;
    }

    if input.status_code >= 400
        && input.response_text.is_some_and(|text| {
            policy
                .error_stop_patterns
                .iter()
                .any(|rule| local_failover_regex_rule_matches(rule, text, input.status_code))
        })
    {
        return LocalFailoverClassification::StopErrorPattern;
    }

    if input.status_code == 200
        && input.response_text.is_some_and(|text| {
            policy
                .success_failover_patterns
                .iter()
                .any(|rule| local_failover_regex_rule_matches(rule, text, input.status_code))
        })
    {
        return LocalFailoverClassification::RetrySuccessPattern;
    }

    if policy.continue_status_codes.contains(&input.status_code) {
        return LocalFailoverClassification::RetryStatusCode;
    }

    if should_failover_local_upstream_status(input.status_code) {
        return LocalFailoverClassification::RetryUpstreamFailure;
    }

    LocalFailoverClassification::UseDefault
}

pub(crate) fn local_failover_error_message(response_text: Option<&str>) -> Option<String> {
    let parsed = parse_local_error_response(response_text);
    parsed
        .message
        .or(parsed.reason)
        .or(parsed.raw)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn should_failover_local_upstream_status(status_code: u16) -> bool {
    status_code >= 400
}

fn is_session_preserving_overload(input: LocalFailoverInput<'_>) -> bool {
    if input.status_code != 503 {
        return false;
    }

    let parsed = parse_local_error_response(input.response_text);
    parsed.message.as_deref().is_some_and(|message| {
        message
            .trim()
            .eq_ignore_ascii_case(SESSION_PRESERVING_OVERLOAD_MESSAGE)
    }) || input
        .response_text
        .is_some_and(|text| text.contains(SESSION_PRESERVING_OVERLOAD_MESSAGE))
}

/// Returns true when a 429 response is explicitly a generic gateway/proxy
/// limiter response and does not identify an account-level Codex quota
/// condition. Official Codex quota errors carry a structured error code (for
/// example `rate_limit_exceeded` or `usage_limit_reached`) and keep the normal
/// quota handling.
pub(crate) fn is_session_preserving_rate_limit(
    status_code: u16,
    response_text: Option<&str>,
) -> bool {
    if status_code != 429 {
        return false;
    }

    let Some(response_text) = response_text else {
        return false;
    };
    let lower = response_text.to_ascii_lowercase();
    if [
        "rate_limit_exceeded",
        "rate_limit_error",
        "usage_limit_reached",
        "usage_not_included",
        "insufficient_quota",
        "quota_exceeded",
        "invalid_token",
        "invalid token",
        "account_locked",
        "banned",
        "suspended",
        "deactivated",
        "disabled",
        "workspace",
        "x-codex-",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return false;
    }

    let parsed = parse_local_error_response(Some(response_text));
    parsed.message.as_deref().is_some_and(|message| {
        message
            .trim()
            .eq_ignore_ascii_case(SESSION_PRESERVING_RATE_LIMIT_MESSAGE)
    }) || parsed.reason.as_deref().is_some_and(|reason| {
        reason
            .trim()
            .eq_ignore_ascii_case(SESSION_PRESERVING_RATE_LIMIT_MESSAGE)
    }) || response_text
        .trim()
        .eq_ignore_ascii_case(SESSION_PRESERVING_RATE_LIMIT_MESSAGE)
        || lower.contains("rate limit exceeded")
}

fn parse_local_error_response(response_text: Option<&str>) -> ParsedLocalErrorResponse {
    let raw = response_text
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let Some(raw_text) = raw.clone() else {
        return ParsedLocalErrorResponse::default();
    };

    let mut parsed = ParsedLocalErrorResponse {
        raw: Some(raw_text.clone()),
        ..ParsedLocalErrorResponse::default()
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw_text) else {
        parsed.message = Some(raw_text);
        return parsed;
    };

    let body_object = value.as_object();
    let error_object = body_object
        .and_then(|object| object.get("error"))
        .and_then(Value::as_object);

    parsed.message = first_non_empty_json_text(error_object, &["message", "detail", "reason"])
        .or_else(|| first_non_empty_json_text(body_object, &["errorMessage"]))
        .or_else(|| {
            body_object
                .and_then(|object| object.get("error"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| first_non_empty_json_text(body_object, &["message", "detail", "reason"]));
    parsed.reason = first_non_empty_json_text(error_object, &["reason", "code", "status"])
        .or_else(|| first_non_empty_json_text(body_object, &["reason", "code", "status"]));

    let Some(message) = parsed.message.clone() else {
        return parsed;
    };
    if !message.starts_with('{') {
        return parsed;
    }

    let Ok(nested) = serde_json::from_str::<Value>(&message) else {
        return parsed;
    };
    let nested_object = nested.as_object();
    let nested_error_object = nested_object
        .and_then(|object| object.get("error"))
        .and_then(Value::as_object);
    parsed.message =
        first_non_empty_json_text(nested_error_object, &["message", "detail", "reason"])
            .or_else(|| first_non_empty_json_text(nested_object, &["message", "detail", "reason"]))
            .or(parsed.message);
    parsed.reason = parsed
        .reason
        .or_else(|| first_non_empty_json_text(nested_error_object, &["reason", "code", "status"]))
        .or_else(|| first_non_empty_json_text(nested_object, &["reason", "code", "status"]));

    parsed
}

fn first_non_empty_json_text(
    object: Option<&serde_json::Map<String, Value>>,
    keys: &[&str],
) -> Option<String> {
    let object = object?;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        match value {
            Value::String(text) if !text.trim().is_empty() => return Some(text.trim().to_string()),
            Value::Number(number) => return Some(number.to_string()),
            _ => {}
        }
    }
    None
}

fn local_failover_regex_rule_matches(
    rule: &LocalFailoverRegexRule,
    response_text: &str,
    status_code: u16,
) -> bool {
    if !rule.status_codes.is_empty() && !rule.status_codes.contains(&status_code) {
        return false;
    }

    Regex::new(&rule.pattern)
        .ok()
        .is_some_and(|regex| regex.is_match(response_text))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        classify_local_failover, is_session_preserving_rate_limit, LocalFailoverClassification,
        LocalFailoverInput, SESSION_PRESERVING_OVERLOAD_MESSAGE,
    };
    use crate::orchestration::{LocalFailoverPolicy, LocalFailoverRegexRule};

    #[test]
    fn classifier_honors_explicit_stop_before_default_retryable_status() {
        let policy = LocalFailoverPolicy {
            stop_status_codes: [503].into_iter().collect(),
            ..LocalFailoverPolicy::default()
        };

        assert_eq!(
            classify_local_failover(&policy, LocalFailoverInput::new(503, None)),
            LocalFailoverClassification::StopStatusCode
        );
    }

    #[test]
    fn classifier_never_fails_over_session_preserving_overload() {
        let policy = LocalFailoverPolicy {
            continue_status_codes: [503].into_iter().collect(),
            ..LocalFailoverPolicy::default()
        };

        for response_text in [
            SESSION_PRESERVING_OVERLOAD_MESSAGE,
            r#"{"error":{"message":"Our servers are currently overloaded. Please try again later."}}"#,
            r#"{"message":"Our servers are currently overloaded. Please try again later."}"#,
            "event: error\ndata: {\"error\":{\"message\":\"Our servers are currently overloaded. Please try again later.\"}}",
        ] {
            assert_eq!(
                classify_local_failover(
                    &policy,
                    LocalFailoverInput::new(503, Some(response_text))
                ),
                LocalFailoverClassification::StopErrorPattern
            );
        }
    }

    #[test]
    fn classifier_still_retries_other_service_unavailable_errors() {
        assert_eq!(
            classify_local_failover(
                &LocalFailoverPolicy::default(),
                LocalFailoverInput::new(
                    503,
                    Some(r#"{"error":{"message":"upstream service unavailable"}}"#)
                )
            ),
            LocalFailoverClassification::RetryUpstreamFailure
        );
    }

    #[test]
    fn generic_rate_limit_is_session_preserving_but_official_quota_is_not() {
        assert!(is_session_preserving_rate_limit(
            429,
            Some(r#"{"detail":"Rate limit exceeded"}"#)
        ));
        assert_eq!(
            classify_local_failover(
                &LocalFailoverPolicy {
                    continue_status_codes: [429].into_iter().collect(),
                    ..LocalFailoverPolicy::default()
                },
                LocalFailoverInput::new(429, Some(r#"{"detail":"Rate limit exceeded"}"#)),
            ),
            LocalFailoverClassification::StopErrorPattern
        );
        assert!(!is_session_preserving_rate_limit(
            429,
            Some(r#"{"error":{"code":"rate_limit_exceeded"}}"#)
        ));
        assert!(!is_session_preserving_rate_limit(
            429,
            Some(r#"{"error":{"type":"rate_limit_error","message":"Rate limit exceeded"}}"#)
        ));
        assert!(!is_session_preserving_rate_limit(429, None));
    }

    #[test]
    fn classifier_detects_success_failover_pattern() {
        let policy = LocalFailoverPolicy {
            success_failover_patterns: vec![LocalFailoverRegexRule {
                pattern: "relay:.*格式错误".to_string(),
                status_codes: BTreeSet::new(),
            }],
            ..LocalFailoverPolicy::default()
        };

        assert_eq!(
            classify_local_failover(
                &policy,
                LocalFailoverInput::new(200, Some("{\"error\":\"relay: 返回格式错误\"}"))
            ),
            LocalFailoverClassification::RetrySuccessPattern
        );
    }

    #[test]
    fn classifier_detects_error_stop_pattern() {
        let policy = LocalFailoverPolicy {
            error_stop_patterns: vec![LocalFailoverRegexRule {
                pattern: "content_policy_violation".to_string(),
                status_codes: [400, 403].into_iter().collect(),
            }],
            ..LocalFailoverPolicy::default()
        };

        assert_eq!(
            classify_local_failover(
                &policy,
                LocalFailoverInput::new(400, Some("{\"error\":\"content_policy_violation\"}"))
            ),
            LocalFailoverClassification::StopErrorPattern
        );
    }

    #[test]
    fn classifier_detects_success_continue_status_code() {
        let policy = LocalFailoverPolicy {
            continue_status_codes: [200].into_iter().collect(),
            ..LocalFailoverPolicy::default()
        };

        assert_eq!(
            classify_local_failover(&policy, LocalFailoverInput::new(200, None)),
            LocalFailoverClassification::RetryStatusCode
        );
    }

    #[test]
    fn classifier_retries_all_error_statuses_without_custom_rule() {
        for (status_code, response_text) in [
            (
                400,
                "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long\"}}",
            ),
            (
                400,
                "{\"error\":{\"message\":\"Unsupported parameter: max_tokens is not supported with this model\"}}",
            ),
            (
                400,
                "{\"error\":{\"message\":\"Unknown parameter: 'tools[0].n'.\"}}",
            ),
            (
                400,
                "{\"error\":{\"message\":\"invalid model for this endpoint\"}}",
            ),
            (
                400,
                "{\"error\":{\"message\":\"invalid `signature` in `thinking` block: signature is for a different request\"}}",
            ),
            (
                400,
                "{\"error\":{\"message\":\"resource_exhausted: quota reached\"}}",
            ),
            (
                401,
                "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Your authentication token has been invalidated. Please try signing in again.\"}}",
            ),
            (
                402,
                "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"payment required: credit balance exhausted\"}}",
            ),
            (
                403,
                "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"verify your account before continuing\"}}",
            ),
            (429, "{\"error\":{\"message\":\"rate limited\"}}"),
            (500, "{\"error\":{\"message\":\"upstream failed\"}}"),
        ] {
            assert_eq!(
                classify_local_failover(
                    &LocalFailoverPolicy::default(),
                    LocalFailoverInput::new(status_code, Some(response_text))
                ),
                LocalFailoverClassification::RetryUpstreamFailure
            );
        }
    }

    #[test]
    fn classifier_keeps_embedded_rate_limit_error_in_success_response_on_default_path() {
        assert_eq!(
            classify_local_failover(
                &LocalFailoverPolicy::default(),
                LocalFailoverInput::new(
                    200,
                    Some(
                        "{\"error\":{\"message\":\"quota reached\",\"type\":\"rate_limit_error\"}}"
                    )
                )
            ),
            LocalFailoverClassification::UseDefault
        );
    }
}
