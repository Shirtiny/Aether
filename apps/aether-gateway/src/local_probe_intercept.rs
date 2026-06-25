use serde_json::Value;
use uuid::Uuid;

use crate::handlers::shared::system_config_bool;
use crate::{AppState, GatewayError};

pub(crate) const LOCAL_PROBE_INTERCEPT_ENABLED_KEY: &str = "module.local_probe_intercept.enabled";
pub(crate) const LOCAL_PROBE_INTERCEPT_RULES_KEY: &str = "module.local_probe_intercept.rules";
pub(crate) const LOCAL_PROBE_INTERCEPT_DELAY_MIN_MS_KEY: &str =
    "module.local_probe_intercept.delay_min_ms";
pub(crate) const LOCAL_PROBE_INTERCEPT_DELAY_MAX_MS_KEY: &str =
    "module.local_probe_intercept.delay_max_ms";

const MAX_PROBE_TEXT_CHARS: usize = 512;
const DEFAULT_DELAY_MIN_MS: u64 = 900;
const DEFAULT_DELAY_MAX_MS: u64 = 2_000;
const MAX_DELAY_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalProbeInterceptKind {
    Ping,
    Health,
}

impl LocalProbeInterceptKind {
    fn from_config(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("health").trim() {
            "ping" => Some(Self::Ping),
            "health" => Some(Self::Health),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalProbeInterceptAnswer {
    pub(crate) text: String,
    pub(crate) kind: LocalProbeInterceptKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalProbeInterceptDelay {
    pub(crate) min_ms: u64,
    pub(crate) max_ms: u64,
}

impl LocalProbeInterceptDelay {
    fn from_bounds(min_ms: u64, max_ms: u64) -> Self {
        let min_ms = min_ms.min(MAX_DELAY_MS);
        let max_ms = max_ms.min(MAX_DELAY_MS);
        if min_ms <= max_ms {
            Self { min_ms, max_ms }
        } else {
            Self {
                min_ms: max_ms,
                max_ms: min_ms,
            }
        }
    }

    pub(crate) fn random_ms(self) -> u64 {
        random_delay_ms(self.min_ms, self.max_ms)
    }
}

#[derive(Debug, Clone)]
struct LocalProbeInterceptRule {
    prompt_key: String,
    response: String,
    kind: LocalProbeInterceptKind,
}

pub(crate) async fn local_probe_intercept_answer(
    state: &AppState,
    text: &str,
) -> Result<Option<LocalProbeInterceptAnswer>, GatewayError> {
    Ok(local_probe_intercept_answer_from_rules(
        text,
        &load_local_probe_intercept_rules(state).await?,
    ))
}

pub(crate) async fn local_probe_intercept_enabled(state: &AppState) -> Result<bool, GatewayError> {
    let value = state
        .read_system_config_json_value(LOCAL_PROBE_INTERCEPT_ENABLED_KEY)
        .await?;
    Ok(system_config_bool(value.as_ref(), true))
}

pub(crate) async fn local_probe_intercept_delay(
    state: &AppState,
) -> Result<LocalProbeInterceptDelay, GatewayError> {
    let min_ms = read_local_probe_delay_ms(
        state,
        LOCAL_PROBE_INTERCEPT_DELAY_MIN_MS_KEY,
        DEFAULT_DELAY_MIN_MS,
    )
    .await?;
    let max_ms = read_local_probe_delay_ms(
        state,
        LOCAL_PROBE_INTERCEPT_DELAY_MAX_MS_KEY,
        DEFAULT_DELAY_MAX_MS,
    )
    .await?;

    Ok(LocalProbeInterceptDelay::from_bounds(min_ms, max_ms))
}

async fn read_local_probe_delay_ms(
    state: &AppState,
    key: &str,
    default_ms: u64,
) -> Result<u64, GatewayError> {
    let value = state.read_system_config_json_value(key).await?;
    Ok(value
        .as_ref()
        .and_then(Value::as_u64)
        .unwrap_or(default_ms)
        .min(MAX_DELAY_MS))
}

async fn load_local_probe_intercept_rules(
    state: &AppState,
) -> Result<Vec<LocalProbeInterceptRule>, GatewayError> {
    let value = state
        .read_system_config_json_value(LOCAL_PROBE_INTERCEPT_RULES_KEY)
        .await?
        .or_else(|| {
            aether_admin::system::admin_system_config_default_value(LOCAL_PROBE_INTERCEPT_RULES_KEY)
        })
        .unwrap_or_else(|| Value::Array(Vec::new()));
    Ok(parse_local_probe_intercept_rules(&value))
}

fn parse_local_probe_intercept_rules(value: &Value) -> Vec<LocalProbeInterceptRule> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            if object
                .get("enabled")
                .and_then(Value::as_bool)
                .is_some_and(|enabled| !enabled)
            {
                return None;
            }
            let prompt = object.get("prompt").and_then(Value::as_str)?.trim();
            let response = object.get("response").and_then(Value::as_str)?.trim();
            if prompt.is_empty() || response.is_empty() {
                return None;
            }
            let kind =
                LocalProbeInterceptKind::from_config(object.get("kind").and_then(Value::as_str))?;
            let prompt_key = local_probe_prompt_key(prompt);
            if prompt_key.is_empty() {
                return None;
            }
            Some(LocalProbeInterceptRule {
                prompt_key,
                response: response.to_string(),
                kind,
            })
        })
        .collect()
}

fn local_probe_intercept_answer_from_rules(
    text: &str,
    rules: &[LocalProbeInterceptRule],
) -> Option<LocalProbeInterceptAnswer> {
    if text.chars().count() > MAX_PROBE_TEXT_CHARS {
        return None;
    }
    let key = local_probe_prompt_key(text);
    rules
        .iter()
        .find(|rule| rule.prompt_key == key)
        .map(|rule| LocalProbeInterceptAnswer {
            text: rule.response.clone(),
            kind: rule.kind,
        })
}

pub(crate) fn local_probe_prompt_key(text: &str) -> String {
    normalize_probe_text(text)
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace() && !is_ignored_probe_punctuation(*ch))
        .collect()
}

fn normalize_probe_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn random_delay_ms(min_ms: u64, max_ms: u64) -> u64 {
    if min_ms >= max_ms {
        return min_ms;
    }
    let span = max_ms - min_ms + 1;
    min_ms + (Uuid::new_v4().as_u128() % u128::from(span)) as u64
}

fn is_ignored_probe_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ','
            | '!'
            | ':'
            | ';'
            | '"'
            | '\''
            | '?'
            | '？'
            | '。'
            | '，'
            | '！'
            | '：'
            | '；'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '、'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_key_ignores_case_space_and_common_punctuation() {
        assert_eq!(
            local_probe_prompt_key("Reply exactly: OK"),
            "replyexactlyok"
        );
        assert_eq!(
            local_probe_prompt_key(" reply  exactly ok "),
            "replyexactlyok"
        );
        assert_eq!(local_probe_prompt_key("你是谁？"), "你是谁");
    }

    #[test]
    fn exact_rule_matching_rejects_long_normal_prompts() {
        let rules = parse_local_probe_intercept_rules(&serde_json::json!([
            {"prompt": "who are you", "response": "I'm ChatGPT.", "kind": "health", "enabled": true}
        ]));
        assert_eq!(
            local_probe_intercept_answer_from_rules("who are you", &rules),
            Some(LocalProbeInterceptAnswer {
                text: "I'm ChatGPT.".to_string(),
                kind: LocalProbeInterceptKind::Health,
            })
        );
        assert_eq!(
            local_probe_intercept_answer_from_rules(
                "Please test whether the parser can answer who are you in a paragraph.",
                &rules,
            ),
            None
        );
    }

    #[test]
    fn delay_bounds_are_normalized_and_capped() {
        assert_eq!(
            LocalProbeInterceptDelay::from_bounds(2_000, 900),
            LocalProbeInterceptDelay {
                min_ms: 900,
                max_ms: 2_000,
            }
        );
        assert_eq!(
            LocalProbeInterceptDelay::from_bounds(120_000, 90_000),
            LocalProbeInterceptDelay {
                min_ms: MAX_DELAY_MS,
                max_ms: MAX_DELAY_MS,
            }
        );
    }

    #[test]
    fn random_delay_stays_inside_bounds() {
        for _ in 0..128 {
            let delay_ms = random_delay_ms(900, 2_000);
            assert!((900..=2_000).contains(&delay_ms));
        }
        assert_eq!(random_delay_ms(1_234, 1_234), 1_234);
    }
}
