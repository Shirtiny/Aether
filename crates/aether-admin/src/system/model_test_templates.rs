use http::header::{HeaderName, HeaderValue};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

pub(super) fn normalize_templates(value: Value) -> Result<Value, &'static str> {
    if value.to_string().len() > 1_048_576 {
        return Err("全局请求模板总大小不能超过 1 MiB");
    }
    let object = value.as_object().ok_or("全局请求模板必须是 JSON 对象")?;
    if object.len() != 2 || !object.contains_key("headers") || !object.contains_key("body") {
        return Err("全局请求模板必须包含 headers 和 body 两个列表");
    }
    let mut result = Map::new();
    for kind in ["headers", "body"] {
        let templates = object[kind].as_array().ok_or("请求模板必须是列表")?;
        if templates.len() > 100 {
            return Err("每类请求模板最多保存 100 个");
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut normalized = Vec::with_capacity(templates.len());
        for template in templates {
            let id = template["id"].as_str().unwrap_or_default().trim();
            let name = template["name"].as_str().unwrap_or_default().trim();
            if id.is_empty() || id.len() > 128 || id.starts_with("__") || !ids.insert(id) {
                return Err("模板 ID 不能为空、重复、以 __ 开头或超过 128 字节");
            }
            if name.is_empty() || name.chars().count() > 100 || !names.insert(name.to_lowercase()) {
                return Err("同类模板名称不能为空、重复或超过 100 个字符");
            }
            let api_format = match template.get("api_format") {
                Some(Value::Null) | None => None,
                Some(Value::String(format)) if format.trim().len() <= 80 => {
                    let format = format.trim().to_ascii_lowercase();
                    (!format.is_empty()).then_some(format)
                }
                _ => return Err("模板端点格式必须为空或不超过 80 字节的字符串"),
            };
            let content = template["content"]
                .as_object()
                .ok_or("模板内容必须是 JSON 对象")?;
            if kind == "headers" {
                for (name, value) in content {
                    HeaderName::from_bytes(name.as_bytes()).map_err(|_| "请求头名称无效")?;
                    let text = match value {
                        Value::String(value) => value.clone(),
                        Value::Number(_) | Value::Bool(_) => value.to_string(),
                        _ => return Err("请求头值必须是字符串、数字或布尔值"),
                    };
                    HeaderValue::from_str(&text).map_err(|_| "请求头值包含无效字符")?;
                }
            }
            normalized.push(json!({
                "id": id,
                "name": name,
                "api_format": api_format,
                "content": content,
            }));
        }
        result.insert(kind.to_string(), Value::Array(normalized));
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::{admin_system_config_default_value, parse_admin_system_config_update};

    fn parse(value: Value) -> Result<Value, (http::StatusCode, Value)> {
        parse_admin_system_config_update(
            "model_test_request_templates",
            &serde_json::to_vec(&json!({ "value": value })).unwrap(),
        )
        .map(|update| update.value)
    }

    fn template() -> Value {
        json!({
            "id": "body-1",
            "name": " 问答测试 ",
            "api_format": " OPENAI:RESPONSES ",
            "content": { "model": "{{model}}", "input": "你好", "stream": true }
        })
    }

    #[test]
    fn model_test_templates_default_is_empty_without_a_migration() {
        assert_eq!(
            admin_system_config_default_value("model_test_request_templates"),
            Some(json!({ "headers": [], "body": [] }))
        );
    }

    #[test]
    fn model_test_templates_normalize_metadata_and_preserve_content() {
        let result = parse(json!({
            "headers": [{ "id": "header-1", "name": "请求头", "content": { "x-test": true, "x-count": 2 } }],
            "body": [template()]
        }))
        .unwrap();
        assert_eq!(result["body"][0]["name"], "问答测试");
        assert_eq!(result["body"][0]["api_format"], "openai:responses");
        assert_eq!(result["body"][0]["content"], template()["content"]);
        assert!(result["headers"][0]["api_format"].is_null());
    }

    #[test]
    fn model_test_templates_reject_invalid_shapes_and_duplicates() {
        for value in [
            Value::Null,
            json!([]),
            json!({}),
            json!({ "headers": [], "body": {} }),
        ] {
            assert_eq!(parse(value).unwrap_err().0, http::StatusCode::BAD_REQUEST);
        }
        for content in [Value::Null, json!([]), json!("not an object")] {
            let mut item = template();
            item["content"] = content;
            assert!(parse(json!({ "headers": [], "body": [item] })).is_err());
        }
        let mut duplicate = template();
        duplicate["id"] = json!("body-2");
        assert!(parse(json!({ "headers": [], "body": [template(), duplicate] })).is_err());
        let mut duplicate = template();
        duplicate["name"] = json!("另一名称");
        assert!(parse(json!({ "headers": [], "body": [template(), duplicate] })).is_err());
    }

    #[test]
    fn model_test_templates_reject_invalid_headers_and_excessive_sizes() {
        for content in [
            json!({ "bad name": "ok" }),
            json!({ "x-test": "bad\r\nheader" }),
            json!({ "x-test": [] }),
        ] {
            let mut item = template();
            item["content"] = content;
            assert!(parse(json!({ "headers": [item], "body": [] })).is_err());
        }
        assert!(parse(json!({ "headers": [], "body": vec![template(); 101] })).is_err());
        let mut item = template();
        item["content"] = json!({ "input": "x".repeat(1_048_576) });
        assert!(parse(json!({ "headers": [], "body": [item] })).is_err());
    }
}
