use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

static GPT56_SOL_OFFICIAL_PRICING: LazyLock<Value> = LazyLock::new(|| {
    serde_json::json!({
        "tiers": [{
            "up_to": null,
            "input_price_per_1m": 5.0,
            "output_price_per_1m": 30.0,
            "cache_creation_price_per_1m": 6.25,
            "cache_read_price_per_1m": 0.5,
            "input_price_per_1m_priority": 10.0,
            "output_price_per_1m_priority": 60.0,
            "cache_creation_price_per_1m_priority": 12.5,
            "cache_read_price_per_1m_priority": 1.0,
            "input_price_per_1m_flex": 2.5,
            "output_price_per_1m_flex": 15.0,
            "cache_creation_price_per_1m_flex": 3.125,
            "cache_read_price_per_1m_flex": 0.25,
            "input_price_per_1m_batches": 2.5,
            "output_price_per_1m_batches": 15.0,
            "cache_creation_price_per_1m_batches": 3.125,
            "cache_read_price_per_1m_batches": 0.25
        }]
    })
});

static GPT56_TERRA_OFFICIAL_PRICING: LazyLock<Value> = LazyLock::new(|| {
    serde_json::json!({
        "tiers": [{
            "up_to": null,
            "input_price_per_1m": 2.5,
            "output_price_per_1m": 15.0,
            "cache_creation_price_per_1m": 3.125,
            "cache_read_price_per_1m": 0.25,
            "input_price_per_1m_priority": 5.0,
            "output_price_per_1m_priority": 30.0,
            "cache_creation_price_per_1m_priority": 6.25,
            "cache_read_price_per_1m_priority": 0.5,
            "input_price_per_1m_flex": 1.25,
            "output_price_per_1m_flex": 7.5,
            "cache_creation_price_per_1m_flex": 1.5625,
            "cache_read_price_per_1m_flex": 0.125,
            "input_price_per_1m_batches": 1.25,
            "output_price_per_1m_batches": 7.5,
            "cache_creation_price_per_1m_batches": 1.5625,
            "cache_read_price_per_1m_batches": 0.125
        }]
    })
});

static GPT56_LUNA_OFFICIAL_PRICING: LazyLock<Value> = LazyLock::new(|| {
    serde_json::json!({
        "tiers": [{
            "up_to": null,
            "input_price_per_1m": 1.0,
            "output_price_per_1m": 6.0,
            "cache_creation_price_per_1m": 1.25,
            "cache_read_price_per_1m": 0.1,
            "input_price_per_1m_priority": 2.0,
            "output_price_per_1m_priority": 12.0,
            "cache_creation_price_per_1m_priority": 2.5,
            "cache_read_price_per_1m_priority": 0.2,
            "input_price_per_1m_flex": 0.5,
            "output_price_per_1m_flex": 3.0,
            "cache_creation_price_per_1m_flex": 0.625,
            "cache_read_price_per_1m_flex": 0.05,
            "input_price_per_1m_batches": 0.5,
            "output_price_per_1m_batches": 3.0,
            "cache_creation_price_per_1m_batches": 0.625,
            "cache_read_price_per_1m_batches": 0.05
        }]
    })
});

fn official_gpt56_pricing(model: &str) -> Option<&'static Value> {
    let normalized = model.trim().to_ascii_lowercase().replace('_', "-");
    for (base, pricing) in [
        ("gpt-5.6-sol", &*GPT56_SOL_OFFICIAL_PRICING),
        ("gpt-5.6-terra", &*GPT56_TERRA_OFFICIAL_PRICING),
        ("gpt-5.6-luna", &*GPT56_LUNA_OFFICIAL_PRICING),
    ] {
        if normalized == base
            || normalized
                .strip_prefix(base)
                .is_some_and(|suffix| suffix.starts_with('-'))
        {
            return Some(pricing);
        }
    }
    None
}

pub const GPT56_LONG_CONTEXT_INPUT_THRESHOLD: i64 = 272_000;
pub const GPT56_LONG_CONTEXT_INPUT_MULTIPLIER: f64 = 2.0;
pub const GPT56_LONG_CONTEXT_OUTPUT_MULTIPLIER: f64 = 1.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingModelPricingSnapshot {
    pub provider_id: String,
    pub provider_billing_type: Option<String>,
    pub provider_api_key_id: Option<String>,
    pub provider_api_key_rate_multipliers: Option<Value>,
    pub provider_api_key_cache_ttl_minutes: Option<i64>,
    pub global_model_id: String,
    pub global_model_name: String,
    pub global_model_config: Option<Value>,
    pub default_price_per_request: Option<f64>,
    pub default_tiered_pricing: Option<Value>,
    pub model_id: Option<String>,
    pub model_provider_model_name: Option<String>,
    pub model_config: Option<Value>,
    pub model_price_per_request: Option<f64>,
    pub model_tiered_pricing: Option<Value>,
}

impl BillingModelPricingSnapshot {
    pub fn is_gpt56(&self) -> bool {
        self.model_provider_model_name
            .as_deref()
            .is_some_and(|model| official_gpt56_pricing(model).is_some())
            || official_gpt56_pricing(&self.global_model_name).is_some()
    }

    pub fn uses_default_gpt56_long_context_policy(&self) -> bool {
        if !self.is_gpt56() {
            return false;
        }
        let Some(tiers) = self
            .effective_tiered_pricing()
            .and_then(|value| value.get("tiers"))
            .and_then(Value::as_array)
        else {
            return false;
        };
        tiers.len() == 1 && tiers[0].get("up_to").is_none_or(serde_json::Value::is_null)
    }

    fn official_fallback_tiered_pricing(&self) -> Option<&'static Value> {
        if self.model_price_per_request.is_some() || self.default_price_per_request.is_some() {
            return None;
        }
        self.model_provider_model_name
            .as_deref()
            .and_then(official_gpt56_pricing)
            .or_else(|| official_gpt56_pricing(&self.global_model_name))
    }

    pub fn effective_tiered_pricing(&self) -> Option<&Value> {
        self.model_tiered_pricing
            .as_ref()
            .filter(|value| has_pricing_data(value))
            .or_else(|| {
                self.default_tiered_pricing
                    .as_ref()
                    .filter(|value| has_pricing_data(value))
            })
            .or_else(|| self.official_fallback_tiered_pricing())
    }

    pub fn effective_price_per_request(&self) -> Option<f64> {
        self.model_price_per_request
            .or(self.default_price_per_request)
    }

    pub fn pricing_source(&self) -> &'static str {
        if self
            .model_tiered_pricing
            .as_ref()
            .is_some_and(has_pricing_data)
            || self.model_price_per_request.is_some()
        {
            "provider_override"
        } else if self
            .default_tiered_pricing
            .as_ref()
            .is_some_and(has_pricing_data)
            || self.default_price_per_request.is_some()
        {
            "global_default"
        } else if self.official_fallback_tiered_pricing().is_some() {
            "official_fallback"
        } else {
            "unpriced"
        }
    }

    pub fn is_free_tier(&self) -> bool {
        self.provider_billing_type
            .as_deref()
            .map(|value| value.eq_ignore_ascii_case("free_tier"))
            .unwrap_or(false)
    }

    pub fn rate_multiplier_for_api_format(&self, api_format: Option<&str>) -> f64 {
        let Some(api_format) = api_format.map(str::trim).filter(|value| !value.is_empty()) else {
            return 1.0;
        };
        let normalized = api_format.to_ascii_lowercase();
        let Some(mapping) = self
            .provider_api_key_rate_multipliers
            .as_ref()
            .and_then(Value::as_object)
        else {
            return 1.0;
        };
        mapping
            .get(&normalized)
            .and_then(|value| value.as_f64())
            .unwrap_or(1.0)
    }
}

fn has_pricing_data(value: &Value) -> bool {
    value
        .get("tiers")
        .and_then(Value::as_array)
        .is_some_and(|tiers| !tiers.is_empty())
        || value
            .get("image_output_price_default")
            .and_then(Value::as_f64)
            .is_some()
        || [
            "image_output_prices",
            "image_output_price_ranges",
            "image_output_price_per_image",
            "image_output_price_matrix",
            "image_prices",
        ]
        .iter()
        .any(|key| value.get(key).is_some_and(value_has_entries))
}

fn value_has_entries(value: &Value) -> bool {
    value.as_object().is_some_and(|object| !object.is_empty())
        || value.as_array().is_some_and(|items| !items.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::BillingModelPricingSnapshot;

    fn snapshot(
        model_tiered_pricing: Option<serde_json::Value>,
        default_tiered_pricing: Option<serde_json::Value>,
    ) -> BillingModelPricingSnapshot {
        BillingModelPricingSnapshot {
            provider_id: "provider-1".to_string(),
            provider_billing_type: None,
            provider_api_key_id: None,
            provider_api_key_rate_multipliers: None,
            provider_api_key_cache_ttl_minutes: None,
            global_model_id: "global-model-1".to_string(),
            global_model_name: "gpt-5".to_string(),
            global_model_config: None,
            default_price_per_request: None,
            default_tiered_pricing,
            model_id: Some("model-1".to_string()),
            model_provider_model_name: Some("gpt-5-upstream".to_string()),
            model_config: None,
            model_price_per_request: None,
            model_tiered_pricing,
        }
    }

    #[test]
    fn empty_provider_tiered_pricing_inherits_global_default() {
        let default_pricing =
            json!({"tiers":[{"up_to":null,"input_price_per_1m":3.0,"output_price_per_1m":15.0}]});
        let pricing = snapshot(Some(json!({})), Some(default_pricing.clone()));

        assert_eq!(pricing.effective_tiered_pricing(), Some(&default_pricing));

        let pricing = snapshot(Some(json!({"tiers": []})), Some(default_pricing.clone()));

        assert_eq!(pricing.effective_tiered_pricing(), Some(&default_pricing));
    }

    #[test]
    fn populated_provider_tiered_pricing_overrides_global_default() {
        let provider_pricing =
            json!({"tiers":[{"up_to":null,"input_price_per_1m":1.0,"output_price_per_1m":2.0}]});
        let default_pricing =
            json!({"tiers":[{"up_to":null,"input_price_per_1m":3.0,"output_price_per_1m":15.0}]});
        let pricing = snapshot(Some(provider_pricing.clone()), Some(default_pricing));

        assert_eq!(pricing.effective_tiered_pricing(), Some(&provider_pricing));
    }

    #[test]
    fn gpt56_models_use_official_fallback_pricing_when_unconfigured() {
        for (model, input, output, cache_write, cache_read) in [
            ("gpt-5.6-sol", 5.0, 30.0, 6.25, 0.5),
            ("gpt-5.6-terra-max", 2.5, 15.0, 3.125, 0.25),
            ("gpt-5.6-luna_preview", 1.0, 6.0, 1.25, 0.1),
        ] {
            let mut pricing = snapshot(None, None);
            pricing.global_model_name = model.to_string();
            let tier = pricing
                .effective_tiered_pricing()
                .and_then(|value| value.get("tiers"))
                .and_then(serde_json::Value::as_array)
                .and_then(|tiers| tiers.first())
                .expect("official GPT-5.6 tier");

            assert_eq!(tier["input_price_per_1m"], input);
            assert_eq!(tier["output_price_per_1m"], output);
            assert_eq!(tier["cache_creation_price_per_1m"], cache_write);
            assert_eq!(tier["cache_read_price_per_1m"], cache_read);
            assert_eq!(pricing.pricing_source(), "official_fallback");
        }
    }

    #[test]
    fn explicit_gpt56_pricing_overrides_official_fallback_including_zero() {
        let explicit = json!({"tiers":[{
            "up_to": null,
            "input_price_per_1m": 4.0,
            "output_price_per_1m": 20.0,
            "cache_creation_price_per_1m": 0.0,
            "cache_read_price_per_1m": 0.4
        }]});
        let mut pricing = snapshot(None, Some(explicit.clone()));
        pricing.global_model_name = "gpt-5.6-sol".to_string();

        assert_eq!(pricing.effective_tiered_pricing(), Some(&explicit));
        assert_eq!(pricing.pricing_source(), "global_default");
    }

    #[test]
    fn explicit_per_request_price_disables_gpt56_token_fallback() {
        let mut pricing = snapshot(None, None);
        pricing.global_model_name = "gpt-5.6-sol".to_string();
        pricing.default_price_per_request = Some(0.02);

        assert!(pricing.effective_tiered_pricing().is_none());
        assert_eq!(pricing.effective_price_per_request(), Some(0.02));
        assert_eq!(pricing.pricing_source(), "global_default");
    }

    #[test]
    fn gpt56_provider_model_name_enables_fallback_for_custom_global_alias() {
        let mut pricing = snapshot(None, None);
        pricing.global_model_name = "custom-sol".to_string();
        pricing.model_provider_model_name = Some("gpt-5.6-sol".to_string());

        assert!(pricing.effective_tiered_pricing().is_some());
        assert_eq!(pricing.pricing_source(), "official_fallback");
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingUsageInput {
    pub task_type: String,
    pub api_format: Option<String>,
    pub service_tier: Option<String>,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_creation_ephemeral_5m_tokens: i64,
    pub cache_creation_ephemeral_1h_tokens: i64,
    pub cache_read_tokens: i64,
    pub image_count: i64,
    pub image_size: Option<String>,
    pub image_quality: Option<String>,
    pub image_output_format: Option<String>,
    pub cache_ttl_minutes: Option<i64>,
}

impl BillingUsageInput {
    pub fn new(task_type: impl Into<String>) -> Self {
        Self {
            task_type: task_type.into(),
            api_format: None,
            service_tier: None,
            request_count: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_ephemeral_5m_tokens: 0,
            cache_creation_ephemeral_1h_tokens: 0,
            cache_read_tokens: 0,
            image_count: 0,
            image_size: None,
            image_quality: None,
            image_output_format: None,
            cache_ttl_minutes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillingComputation {
    pub cost_result: crate::CostResult,
    pub actual_total_cost: f64,
    pub rate_multiplier: f64,
    pub is_free_tier: bool,
}
