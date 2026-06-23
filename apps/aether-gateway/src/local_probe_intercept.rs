use serde_json::Value;

use crate::handlers::shared::system_config_bool;
use crate::{AppState, GatewayError};

pub(crate) const LOCAL_PROBE_INTERCEPT_ENABLED_KEY: &str = "module.local_probe_intercept.enabled";
pub(crate) const LOCAL_PROBE_INTERCEPT_RULES_KEY: &str = "module.local_probe_intercept.rules";

const MAX_PROBE_TEXT_CHARS: usize = 512;

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
}
