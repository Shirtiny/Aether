use crate::image_capabilities::{
    openai_image_normalize_options_for_provider, openai_image_provider_max_generation_count,
};
use serde_json::{json, Value};

const GROK_IMAGE_MODEL_IDS: &[&str] = &[
    "grok-imagine-image-lite",
    "grok-imagine-image",
    "grok-imagine-image-quality",
    "grok-imagine-image-pro",
    "grok-imagine-edit",
    "grok-imagine-image-edit",
];
const GROK_IMAGE_EDIT_MODEL_IDS: &[&str] = &["grok-imagine-edit", "grok-imagine-image-edit"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdminProviderOpenAiImageTestCapability {
    pub(crate) max_generation_count: u64,
}

impl AdminProviderOpenAiImageTestCapability {
    pub(crate) fn supports_generation_count(self, count: u64) -> bool {
        count >= 1 && count <= self.max_generation_count
    }
}

pub(crate) fn admin_provider_openai_image_test_capability(
    provider_type: &str,
) -> AdminProviderOpenAiImageTestCapability {
    AdminProviderOpenAiImageTestCapability {
        max_generation_count: openai_image_provider_max_generation_count(provider_type),
    }
}

pub(crate) fn admin_provider_openai_image_normalize_options(
    provider_type: &str,
) -> crate::ai_serving::OpenAiImageNormalizeOptions {
    openai_image_normalize_options_for_provider(provider_type)
}

pub(crate) fn admin_provider_model_test_capabilities_payload(
    provider_type: &str,
    model_id: &str,
    supports_image_generation: bool,
) -> Value {
    let provider_type = provider_type.trim();
    let model_id = model_id.trim();
    let is_grok_image_edit = provider_type.eq_ignore_ascii_case("grok")
        && GROK_IMAGE_EDIT_MODEL_IDS
            .iter()
            .any(|candidate| model_id.eq_ignore_ascii_case(candidate));
    let openai_image = if supports_image_generation {
        Some(json!({
            "max_generation_count": admin_provider_openai_image_test_capability(provider_type).max_generation_count,
            "supports_generation": !is_grok_image_edit,
            "supports_edit": is_grok_image_edit,
        }))
    } else {
        None
    };

    json!({
        "openai:image": openai_image,
    })
}

pub(crate) fn admin_provider_model_supports_image_generation(
    provider_type: &str,
    model_id: &str,
    fallback_supports_image_generation: bool,
) -> bool {
    if provider_type.trim().eq_ignore_ascii_case("grok") {
        let model_id = model_id.trim();
        return GROK_IMAGE_MODEL_IDS
            .iter()
            .any(|candidate| model_id.eq_ignore_ascii_case(candidate));
    }
    fallback_supports_image_generation
}

pub(crate) fn admin_provider_model_is_image_edit_only(provider_type: &str, model_id: &str) -> bool {
    provider_type.trim().eq_ignore_ascii_case("grok")
        && GROK_IMAGE_EDIT_MODEL_IDS
            .iter()
            .any(|candidate| model_id.trim().eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_image_generation_models_expose_multi_image_capability() {
        let payload =
            admin_provider_model_test_capabilities_payload("grok", "grok-imagine-image", true);

        assert_eq!(payload["openai:image"]["max_generation_count"], 4);
        assert_eq!(payload["openai:image"]["supports_generation"], true);
        assert_eq!(payload["openai:image"]["supports_edit"], false);
    }

    #[test]
    fn grok_image_edit_model_is_edit_only_for_generation_tests() {
        for model in ["grok-imagine-edit", "grok-imagine-image-edit"] {
            assert!(admin_provider_model_is_image_edit_only("grok", model));
            let payload = admin_provider_model_test_capabilities_payload("grok", model, true);

            assert_eq!(payload["openai:image"]["max_generation_count"], 4);
            assert_eq!(payload["openai:image"]["supports_generation"], false);
            assert_eq!(payload["openai:image"]["supports_edit"], true);
        }
        assert!(!admin_provider_model_is_image_edit_only(
            "grok",
            "grok-imagine-image"
        ));
    }

    #[test]
    fn non_image_models_report_null_image_test_capability() {
        let payload = admin_provider_model_test_capabilities_payload("openai", "gpt-5.5", false);

        assert!(payload["openai:image"].is_null());
    }

    #[test]
    fn grok_image_support_uses_catalog_model_ids_not_global_fallback() {
        assert!(admin_provider_model_supports_image_generation(
            "grok",
            "grok-imagine-image-pro",
            false,
        ));
        assert!(admin_provider_model_supports_image_generation(
            "grok",
            "grok-imagine-image-quality",
            false,
        ));
        assert!(admin_provider_model_supports_image_generation(
            "grok",
            "grok-imagine-edit",
            false,
        ));
        assert!(!admin_provider_model_supports_image_generation(
            "grok",
            "grok-4.20-fast",
            true,
        ));
    }
}
