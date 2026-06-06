pub(crate) fn normalized_signature(api_format: &str) -> Option<&'static str> {
    match crate::ai_serving::normalize_api_format_alias(api_format).as_str() {
        "aliyun:multimodal_embedding" => Some("aliyun:multimodal_embedding"),
        _ => None,
    }
}

pub(crate) fn local_path(api_format: &str) -> Option<&'static str> {
    match crate::ai_serving::normalize_api_format_alias(api_format).as_str() {
        "aliyun:multimodal_embedding" => {
            Some("/api/v1/services/embeddings/multimodal-embedding/multimodal-embedding")
        }
        _ => None,
    }
}
