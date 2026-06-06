use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::default_rule::{
    explicit_image_output_price_default, explicit_image_output_price_entries,
    explicit_image_output_price_ranges, normalize_task_type, DefaultBillingRuleGenerator,
};
use crate::precision::quantize_cost;
use crate::pricing::{BillingComputation, BillingModelPricingSnapshot, BillingUsageInput};
use crate::schema::{
    BillingSnapshot, BillingSnapshotStatus, CostResult, BILLING_SNAPSHOT_SCHEMA_VERSION,
};
use crate::{
    normalize_input_tokens_for_billing, normalize_total_input_context_for_cache_hit_rate,
    ExpressionEvaluationError, FormulaEngine, FormulaEvaluationStatus,
};

pub struct BillingService {
    engine: FormulaEngine,
}

impl BillingService {
    pub fn new() -> Self {
        Self {
            engine: FormulaEngine::new(),
        }
    }

    pub fn calculate(
        &self,
        pricing: &BillingModelPricingSnapshot,
        input: &BillingUsageInput,
    ) -> Result<BillingComputation, ExpressionEvaluationError> {
        let Some(rule) =
            DefaultBillingRuleGenerator::generate_for_pricing(pricing, &input.task_type)
        else {
            return Ok(BillingComputation {
                cost_result: CostResult {
                    cost: 0.0,
                    status: BillingSnapshotStatus::NoRule,
                    snapshot: BillingSnapshot {
                        schema_version: BILLING_SNAPSHOT_SCHEMA_VERSION.to_string(),
                        rule_id: None,
                        rule_name: None,
                        scope: None,
                        expression: None,
                        resolved_dimensions: build_dimensions(input, pricing),
                        resolved_variables: BTreeMap::new(),
                        cost_breakdown: BTreeMap::new(),
                        total_cost: 0.0,
                        tier_index: None,
                        tier_info: None,
                        missing_required: Vec::new(),
                        status: BillingSnapshotStatus::NoRule,
                        calculated_at: now_marker(),
                        engine_version: "2.0".to_string(),
                    },
                },
                actual_total_cost: 0.0,
                rate_multiplier: pricing
                    .rate_multiplier_for_api_format(input.api_format.as_deref()),
                is_free_tier: pricing.is_free_tier(),
            });
        };

        let dims = build_dimensions(input, pricing);
        let result = self.engine.evaluate(
            &rule.expression,
            Some(&rule.variables),
            Some(&dims),
            Some(&rule.dimension_mappings),
            false,
        )?;

        let status = match result.status {
            FormulaEvaluationStatus::Complete => BillingSnapshotStatus::Complete,
            FormulaEvaluationStatus::Incomplete => BillingSnapshotStatus::Incomplete,
        };
        let total_cost = if matches!(status, BillingSnapshotStatus::Complete) {
            result.cost
        } else {
            0.0
        };
        let rate_multiplier = pricing.rate_multiplier_for_api_format(input.api_format.as_deref());
        let is_free_tier = pricing.is_free_tier();
        let actual_total_cost = if is_free_tier {
            0.0
        } else {
            quantize_cost(total_cost * rate_multiplier)
        };

        Ok(BillingComputation {
            cost_result: CostResult {
                cost: total_cost,
                status,
                snapshot: BillingSnapshot {
                    schema_version: BILLING_SNAPSHOT_SCHEMA_VERSION.to_string(),
                    rule_id: Some(rule.id),
                    rule_name: Some(rule.name),
                    scope: Some(rule.scope),
                    expression: Some(rule.expression),
                    resolved_dimensions: result.resolved_dimensions,
                    resolved_variables: result.resolved_variables,
                    cost_breakdown: result.cost_breakdown,
                    total_cost,
                    tier_index: result.tier_index,
                    tier_info: result.tier_info,
                    missing_required: result.missing_required,
                    status,
                    calculated_at: now_marker(),
                    engine_version: "2.0".to_string(),
                },
            },
            actual_total_cost,
            rate_multiplier,
            is_free_tier,
        })
    }
}

impl Default for BillingService {
    fn default() -> Self {
        Self::new()
    }
}

fn build_dimensions(
    input: &BillingUsageInput,
    pricing: &BillingModelPricingSnapshot,
) -> BTreeMap<String, Value> {
    let normalized_input_tokens = normalize_input_tokens_for_billing(
        input.api_format.as_deref(),
        input.input_tokens,
        input.cache_read_tokens,
    );
    let classified_cache_creation_tokens = input
        .cache_creation_ephemeral_5m_tokens
        .saturating_add(input.cache_creation_ephemeral_1h_tokens);
    let cache_creation_uncategorized_tokens = input
        .cache_creation_tokens
        .saturating_sub(classified_cache_creation_tokens)
        .max(0);
    let total_input_context = normalize_total_input_context_for_cache_hit_rate(
        input.api_format.as_deref(),
        input.input_tokens,
        input.cache_creation_tokens,
        input.cache_read_tokens,
    );
    let image_output_pricing = image_output_pricing_state(pricing);
    let image_output_resolution = resolve_image_output_price_resolution(pricing, input);

    let mut out = BTreeMap::from([
        ("input_tokens".to_string(), json!(normalized_input_tokens)),
        ("output_tokens".to_string(), json!(input.output_tokens)),
        (
            "cache_creation_tokens".to_string(),
            json!(input.cache_creation_tokens),
        ),
        (
            "cache_creation_ephemeral_5m_tokens".to_string(),
            json!(input.cache_creation_ephemeral_5m_tokens),
        ),
        (
            "cache_creation_ephemeral_1h_tokens".to_string(),
            json!(input.cache_creation_ephemeral_1h_tokens),
        ),
        (
            "cache_creation_uncategorized_tokens".to_string(),
            json!(cache_creation_uncategorized_tokens),
        ),
        (
            "cache_read_tokens".to_string(),
            json!(input.cache_read_tokens),
        ),
        (
            "request_count".to_string(),
            json!(input.request_count.max(0)),
        ),
        ("image_count".to_string(), json!(input.image_count.max(0))),
        (
            "image_count_unmetered".to_string(),
            json!(if image_output_pricing.enabled {
                input.image_count.max(0)
            } else {
                0
            }),
        ),
        (
            "image_output_pricing_enabled".to_string(),
            json!(image_output_pricing.enabled),
        ),
        (
            "image_output_matrix_enabled".to_string(),
            json!(image_output_pricing.matrix_enabled),
        ),
        (
            "image_output_range_enabled".to_string(),
            json!(image_output_pricing.range_enabled),
        ),
        (
            "image_output_pricing_mode".to_string(),
            json!(image_output_resolution.pricing_mode),
        ),
        (
            "image_output_price_per_image".to_string(),
            json!(image_output_resolution.price_per_image),
        ),
        (
            "total_input_context".to_string(),
            json!(total_input_context),
        ),
        (
            "effective_task_type".to_string(),
            json!(normalize_task_type(&input.task_type)),
        ),
    ]);

    out.insert(
        "cache_creation_ephemeral_5m_ttl_minutes".to_string(),
        json!(5),
    );
    out.insert(
        "cache_creation_ephemeral_1h_ttl_minutes".to_string(),
        json!(60),
    );

    if let Some(cache_ttl_minutes) = input.cache_ttl_minutes {
        out.insert(
            "cache_ttl_minutes".to_string(),
            json!(cache_ttl_minutes.max(0)),
        );
    }
    if let Some(image_pixels) = image_output_resolution.image_pixels {
        out.insert("image_pixels".to_string(), json!(image_pixels));
    }
    if let Some(price_bucket) = image_output_resolution.price_bucket.as_ref() {
        out.insert("image_output_price_bucket".to_string(), json!(price_bucket));
    }
    if input.image_count > 0 {
        let image_size = input
            .image_size
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let image_quality = input
            .image_quality
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        if let Some(image_size) = image_size.as_ref() {
            out.insert("image_size".to_string(), json!(image_size));
        }
        if let Some(image_quality) = image_quality.as_ref() {
            out.insert("image_quality".to_string(), json!(image_quality));
        }
        if let (Some(image_size), Some(image_quality)) =
            (image_size.as_ref(), image_quality.as_ref())
        {
            out.insert(
                "image_price_key".to_string(),
                json!(format!(
                    "{}:{}",
                    normalize_image_output_size(image_size),
                    normalize_image_output_quality(image_quality)
                )),
            );
        }
    }
    if let Some(output_format) = input
        .image_output_format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.insert("image_output_format".to_string(), json!(output_format));
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct ImageOutputPricingState {
    enabled: bool,
    matrix_enabled: bool,
    range_enabled: bool,
}

#[derive(Debug, Clone)]
struct ImageOutputPriceResolution {
    price_per_image: f64,
    pricing_mode: &'static str,
    price_bucket: Option<String>,
    image_pixels: Option<i64>,
}

#[derive(Debug, Clone)]
struct ParsedImageOutputPriceRange {
    up_to_pixels: Option<i64>,
    label: Option<String>,
    prices: BTreeMap<String, f64>,
}

fn image_output_pricing_state(pricing: &BillingModelPricingSnapshot) -> ImageOutputPricingState {
    let matrix_enabled = pricing_has_image_output_matrix(pricing);
    let range_enabled = pricing_has_image_output_ranges(pricing);
    let default_enabled = pricing_has_image_output_default_price(pricing);
    ImageOutputPricingState {
        enabled: matrix_enabled || range_enabled || default_enabled,
        matrix_enabled,
        range_enabled,
    }
}

fn resolve_image_output_price_resolution(
    pricing: &BillingModelPricingSnapshot,
    input: &BillingUsageInput,
) -> ImageOutputPriceResolution {
    let pricing_config = pricing.effective_tiered_pricing();
    let default_price = explicit_image_output_price_default(pricing_config);
    let image_size = input
        .image_size
        .as_deref()
        .map(normalize_image_output_size)
        .filter(|value| !value.is_empty());
    let image_quality = input
        .image_quality
        .as_deref()
        .map(normalize_image_output_quality)
        .filter(|value| !value.is_empty());
    let image_pixels = image_size.as_deref().and_then(parse_image_size_pixels);

    if let (Some(size), Some(entries)) = (
        image_size.as_deref(),
        explicit_image_output_price_entries(pricing_config),
    ) {
        for key in image_price_lookup_keys(size, image_quality.as_deref()) {
            if let Some(price) = entries.get(&key).and_then(Value::as_f64) {
                return ImageOutputPriceResolution {
                    price_per_image: price,
                    pricing_mode: "matrix",
                    price_bucket: None,
                    image_pixels,
                };
            }
        }
    }

    if let Some(pixels) = image_pixels {
        if let Some((price, bucket)) = resolve_image_output_range_price(
            explicit_image_output_price_ranges(pricing_config).unwrap_or_default(),
            pixels,
            image_quality.as_deref(),
            default_price,
        ) {
            return ImageOutputPriceResolution {
                price_per_image: price,
                pricing_mode: "pixel_tiers",
                price_bucket: Some(bucket),
                image_pixels,
            };
        }
    }

    if let Some(price) = default_price {
        return ImageOutputPriceResolution {
            price_per_image: price,
            pricing_mode: "per_image",
            price_bucket: Some("default".to_string()),
            image_pixels,
        };
    }

    ImageOutputPriceResolution {
        price_per_image: 0.0,
        pricing_mode: "none",
        price_bucket: None,
        image_pixels,
    }
}

fn pricing_has_image_output_matrix(pricing: &BillingModelPricingSnapshot) -> bool {
    let Some(config) = pricing.effective_tiered_pricing() else {
        return false;
    };
    [
        "image_output_prices",
        "image_output_price_per_image",
        "image_output_price_matrix",
        "image_prices",
    ]
    .iter()
    .any(|key| {
        config
            .get(key)
            .is_some_and(image_price_entries_have_matrix_values)
    })
}

fn pricing_has_image_output_ranges(pricing: &BillingModelPricingSnapshot) -> bool {
    explicit_image_output_price_ranges(pricing.effective_tiered_pricing())
        .is_some_and(|ranges| !ranges.is_empty())
}

fn pricing_has_image_output_default_price(pricing: &BillingModelPricingSnapshot) -> bool {
    let Some(config) = pricing.effective_tiered_pricing() else {
        return false;
    };
    config
        .get("image_output_price_default")
        .or_else(|| config.get("image_price_default"))
        .or_else(|| {
            config
                .get("image_output_prices")
                .and_then(|value| value.get("default"))
        })
        .and_then(Value::as_f64)
        .is_some()
}

fn image_price_entries_have_matrix_values(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            !key.eq_ignore_ascii_case("default")
                && (value.as_f64().is_some() || image_price_entries_have_matrix_values(value))
        }),
        Value::Array(items) => items.iter().any(image_price_entries_have_matrix_values),
        _ => false,
    }
}

fn resolve_image_output_range_price(
    ranges: Vec<Value>,
    image_pixels: i64,
    image_quality: Option<&str>,
    default_price: Option<f64>,
) -> Option<(f64, String)> {
    let mut parsed_ranges = ranges
        .iter()
        .filter_map(parse_image_output_price_range)
        .collect::<Vec<_>>();
    parsed_ranges.sort_by(
        |left, right| match (left.up_to_pixels, right.up_to_pixels) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    );

    for range in parsed_ranges {
        if !range
            .up_to_pixels
            .map(|up_to| image_pixels <= up_to)
            .unwrap_or(true)
        {
            continue;
        }
        let Some(price) =
            image_output_price_for_quality(&range.prices, image_quality).or(default_price)
        else {
            continue;
        };
        return Some((price, image_output_range_bucket(&range)));
    }

    None
}

fn parse_image_output_price_range(value: &Value) -> Option<ParsedImageOutputPriceRange> {
    let object = value.as_object()?;
    let prices = object
        .get("prices")
        .and_then(Value::as_object)?
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_f64()
                .map(|price| (key.to_ascii_lowercase(), price))
        })
        .collect::<BTreeMap<_, _>>();
    if prices.is_empty() {
        return None;
    }
    Some(ParsedImageOutputPriceRange {
        up_to_pixels: object.get("up_to_pixels").and_then(value_as_positive_i64),
        label: object
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        prices,
    })
}

fn image_output_price_for_quality(
    prices: &BTreeMap<String, f64>,
    image_quality: Option<&str>,
) -> Option<f64> {
    for key in image_quality_lookup_keys(image_quality) {
        if let Some(price) = prices.get(&key) {
            return Some(*price);
        }
    }
    None
}

fn image_price_lookup_keys(size: &str, image_quality: Option<&str>) -> Vec<String> {
    image_quality_lookup_keys(image_quality)
        .into_iter()
        .filter(|quality| quality != "default")
        .map(|quality| format!("{}:{}", size, quality))
        .collect()
}

fn image_quality_lookup_keys(image_quality: Option<&str>) -> Vec<String> {
    let quality = image_quality
        .map(normalize_image_output_quality)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "medium".to_string());
    let mut keys = vec![quality.clone()];
    if quality == "auto" {
        keys.push("medium".to_string());
    }
    keys.push("default".to_string());
    keys
}

fn image_output_range_bucket(range: &ParsedImageOutputPriceRange) -> String {
    range
        .label
        .clone()
        .unwrap_or_else(|| match range.up_to_pixels {
            Some(up_to_pixels) => format!("<={up_to_pixels}px"),
            None => "unbounded".to_string(),
        })
}

fn normalize_image_output_size(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace('×', "x")
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

fn normalize_image_output_quality(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn parse_image_size_pixels(size: &str) -> Option<i64> {
    let (width, height) = size.split_once('x')?;
    let width = width.parse::<i64>().ok()?;
    let height = height.parse::<i64>().ok()?;
    if width <= 0 || height <= 0 {
        return None;
    }
    width.checked_mul(height)
}

fn value_as_positive_i64(value: &Value) -> Option<i64> {
    let parsed = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))?;
    (parsed > 0).then_some(parsed)
}

fn now_marker() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::BillingService;
    use crate::{BillingModelPricingSnapshot, BillingSnapshotStatus, BillingUsageInput};

    fn pricing() -> BillingModelPricingSnapshot {
        BillingModelPricingSnapshot {
            provider_id: "provider-1".to_string(),
            provider_billing_type: Some("pay_as_you_go".to_string()),
            provider_api_key_id: Some("key-1".to_string()),
            provider_api_key_rate_multipliers: Some(json!({"openai:chat": 0.5})),
            provider_api_key_cache_ttl_minutes: Some(60),
            global_model_id: "global-model-1".to_string(),
            global_model_name: "gpt-5".to_string(),
            global_model_config: None,
            default_price_per_request: Some(0.02),
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 3.0,
                    "output_price_per_1m": 15.0,
                    "cache_creation_price_per_1m": 3.75,
                    "cache_read_price_per_1m": 0.30
                }]
            })),
            model_id: Some("model-1".to_string()),
            model_provider_model_name: Some("gpt-5-upstream".to_string()),
            model_config: None,
            model_price_per_request: None,
            model_tiered_pricing: None,
        }
    }

    #[test]
    fn calculates_complete_snapshot_for_usage() {
        let result = BillingService::new()
            .calculate(
                &pricing(),
                &BillingUsageInput {
                    task_type: "chat".to_string(),
                    api_format: Some("openai:chat".to_string()),
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 500,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 100,
                    image_count: 0,
                    image_size: None,
                    image_quality: None,
                    image_output_format: None,
                    cache_ttl_minutes: Some(60),
                },
            )
            .expect("billing should calculate");

        assert_eq!(result.cost_result.status, BillingSnapshotStatus::Complete);
        assert!(result.cost_result.cost > 0.0);
        assert!(result.actual_total_cost > 0.0);
        assert_eq!(result.rate_multiplier, 0.5);
    }

    #[test]
    fn openai_cache_hit_context_does_not_double_count_cache_read() {
        let result = BillingService::new()
            .calculate(
                &pricing(),
                &BillingUsageInput {
                    task_type: "chat".to_string(),
                    api_format: Some("openai:responses".to_string()),
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 10,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 800,
                    image_count: 0,
                    image_size: None,
                    image_quality: None,
                    image_output_format: None,
                    cache_ttl_minutes: Some(60),
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("input_tokens"),
            Some(&json!(200))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("total_input_context"),
            Some(&json!(1_000))
        );
    }

    #[test]
    fn image_token_usage_without_image_output_price_bills_tokens_only() {
        let pricing = BillingModelPricingSnapshot {
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 1.0,
                    "output_price_per_1m": 2.0
                }]
            })),
            ..pricing()
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "image".to_string(),
                    api_format: Some("openai:image".to_string()),
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 20_000,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 0,
                    image_count: 1,
                    image_size: Some("1024x1024".to_string()),
                    image_quality: Some("medium".to_string()),
                    image_output_format: Some("png".to_string()),
                    cache_ttl_minutes: None,
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_output_pricing_mode"),
            Some(&json!("none"))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_count_unmetered"),
            Some(&json!(0))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .cost_breakdown
                .get("image_output_cost"),
            Some(&0.0)
        );
        assert_eq!(result.cost_result.cost, 0.041);
    }

    #[test]
    fn image_default_output_price_adds_image_cost_even_with_token_usage() {
        let pricing = BillingModelPricingSnapshot {
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 1.0,
                    "output_price_per_1m": 2.0
                }],
                "image_output_price_default": 0.05
            })),
            ..pricing()
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "image".to_string(),
                    api_format: Some("openai:image".to_string()),
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 20_000,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 0,
                    image_count: 1,
                    image_size: Some("1024x1024".to_string()),
                    image_quality: Some("medium".to_string()),
                    image_output_format: Some("png".to_string()),
                    cache_ttl_minutes: None,
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_output_pricing_mode"),
            Some(&json!("per_image"))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .cost_breakdown
                .get("image_output_cost"),
            Some(&0.05)
        );
        assert_eq!(result.cost_result.cost, 0.091);
    }

    #[test]
    fn image_default_output_price_generates_rule_without_token_tiers() {
        let pricing = BillingModelPricingSnapshot {
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "image_output_price_default": 0.05
            })),
            ..pricing()
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "image".to_string(),
                    api_format: Some("openai:image".to_string()),
                    request_count: 1,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 0,
                    image_count: 2,
                    image_size: Some("1024x1024".to_string()),
                    image_quality: Some("medium".to_string()),
                    image_output_format: Some("png".to_string()),
                    cache_ttl_minutes: None,
                },
            )
            .expect("billing should calculate");

        assert_eq!(result.cost_result.status, BillingSnapshotStatus::Complete);
        assert_eq!(
            result
                .cost_result
                .snapshot
                .cost_breakdown
                .get("image_output_cost"),
            Some(&0.1)
        );
        assert_eq!(result.cost_result.cost, 0.1);
    }

    #[test]
    fn image_pixel_ranges_generate_rule_without_token_tiers() {
        let pricing = BillingModelPricingSnapshot {
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "image_output_price_ranges": [{
                    "up_to_pixels": null,
                    "prices": { "medium": 0.04 }
                }]
            })),
            ..pricing()
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "image".to_string(),
                    api_format: Some("openai:image".to_string()),
                    request_count: 1,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 0,
                    image_count: 2,
                    image_size: Some("1024x1024".to_string()),
                    image_quality: Some("medium".to_string()),
                    image_output_format: Some("png".to_string()),
                    cache_ttl_minutes: None,
                },
            )
            .expect("billing should calculate");

        assert_eq!(result.cost_result.status, BillingSnapshotStatus::Complete);
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_output_pricing_mode"),
            Some(&json!("pixel_tiers"))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .cost_breakdown
                .get("image_output_cost"),
            Some(&0.08)
        );
        assert_eq!(result.cost_result.cost, 0.08);
    }

    #[test]
    fn image_token_usage_with_matrix_adds_matrix_image_cost() {
        let pricing = BillingModelPricingSnapshot {
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 1.0,
                    "output_price_per_1m": 2.0
                }],
                "image_output_price_default": 0.01,
                "image_output_prices": {
                    "1024x1024": { "medium": 0.05 }
                }
            })),
            ..pricing()
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "image".to_string(),
                    api_format: Some("openai:image".to_string()),
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 20_000,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 0,
                    image_count: 1,
                    image_size: Some("1024x1024".to_string()),
                    image_quality: Some("medium".to_string()),
                    image_output_format: Some("png".to_string()),
                    cache_ttl_minutes: None,
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_output_pricing_mode"),
            Some(&json!("matrix"))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .cost_breakdown
                .get("image_output_cost"),
            Some(&0.05)
        );
    }

    #[test]
    fn image_token_usage_with_pixel_ranges_adds_range_image_cost() {
        let pricing = BillingModelPricingSnapshot {
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 1.0,
                    "output_price_per_1m": 2.0
                }],
                "image_output_price_default": 0.01,
                "image_output_price_ranges": [
                    {
                        "up_to_pixels": 1_048_576,
                        "prices": { "medium": 0.04 }
                    },
                    {
                        "up_to_pixels": 2_097_152,
                        "prices": { "medium": 0.08 }
                    }
                ]
            })),
            ..pricing()
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "image".to_string(),
                    api_format: Some("openai:image".to_string()),
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 20_000,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 0,
                    image_count: 1,
                    image_size: Some("1536 x 1024".to_string()),
                    image_quality: Some("medium".to_string()),
                    image_output_format: Some("png".to_string()),
                    cache_ttl_minutes: None,
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_output_pricing_mode"),
            Some(&json!("pixel_tiers"))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_pixels"),
            Some(&json!(1_572_864))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_dimensions
                .get("image_output_price_bucket"),
            Some(&json!("<=2097152px"))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_variables
                .get("image_output_price_per_image"),
            Some(&json!(0.08))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .cost_breakdown
                .get("image_output_cost"),
            Some(&0.08)
        );
        assert_eq!(result.cost_result.cost, 0.121);
    }

    #[test]
    fn five_minute_cache_ttl_uses_base_cache_prices() {
        let pricing = BillingModelPricingSnapshot {
            provider_id: "provider-1".to_string(),
            provider_billing_type: Some("pay_as_you_go".to_string()),
            provider_api_key_id: Some("key-1".to_string()),
            provider_api_key_rate_multipliers: None,
            provider_api_key_cache_ttl_minutes: Some(5),
            global_model_id: "global-model-1".to_string(),
            global_model_name: "gpt-5.4".to_string(),
            global_model_config: None,
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 2.5,
                    "output_price_per_1m": 15.0,
                    "cache_creation_price_per_1m": 3.125,
                    "cache_read_price_per_1m": 0.25,
                    "cache_ttl_pricing": [{
                        "ttl_minutes": 60,
                        "cache_creation_price_per_1m": 5.0,
                        "cache_read_price_per_1m": null
                    }]
                }]
            })),
            model_id: None,
            model_provider_model_name: None,
            model_config: None,
            model_price_per_request: None,
            model_tiered_pricing: None,
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "chat".to_string(),
                    api_format: None,
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 10,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 100,
                    image_count: 0,
                    image_size: None,
                    image_quality: None,
                    image_output_format: None,
                    cache_ttl_minutes: Some(5),
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_variables
                .get("cache_creation_price_per_1m"),
            Some(&json!(3.125))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_variables
                .get("cache_read_price_per_1m"),
            Some(&json!(0.25))
        );
    }

    #[test]
    fn one_hour_cache_ttl_keeps_base_cache_read_when_ttl_entry_omits_it() {
        let pricing = BillingModelPricingSnapshot {
            provider_id: "provider-1".to_string(),
            provider_billing_type: Some("pay_as_you_go".to_string()),
            provider_api_key_id: Some("key-1".to_string()),
            provider_api_key_rate_multipliers: None,
            provider_api_key_cache_ttl_minutes: Some(60),
            global_model_id: "global-model-1".to_string(),
            global_model_name: "gpt-5.4".to_string(),
            global_model_config: None,
            default_price_per_request: None,
            default_tiered_pricing: Some(json!({
                "tiers": [{
                    "up_to": null,
                    "input_price_per_1m": 2.5,
                    "output_price_per_1m": 15.0,
                    "cache_creation_price_per_1m": 3.125,
                    "cache_read_price_per_1m": 0.25,
                    "cache_ttl_pricing": [{
                        "ttl_minutes": 60,
                        "cache_creation_price_per_1m": 5.0,
                        "cache_read_price_per_1m": null
                    }]
                }]
            })),
            model_id: None,
            model_provider_model_name: None,
            model_config: None,
            model_price_per_request: None,
            model_tiered_pricing: None,
        };

        let result = BillingService::new()
            .calculate(
                &pricing,
                &BillingUsageInput {
                    task_type: "chat".to_string(),
                    api_format: None,
                    request_count: 1,
                    input_tokens: 1_000,
                    output_tokens: 10,
                    cache_creation_tokens: 0,
                    cache_creation_ephemeral_5m_tokens: 0,
                    cache_creation_ephemeral_1h_tokens: 0,
                    cache_read_tokens: 100,
                    image_count: 0,
                    image_size: None,
                    image_quality: None,
                    image_output_format: None,
                    cache_ttl_minutes: Some(60),
                },
            )
            .expect("billing should calculate");

        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_variables
                .get("cache_creation_price_per_1m"),
            Some(&json!(5.0))
        );
        assert_eq!(
            result
                .cost_result
                .snapshot
                .resolved_variables
                .get("cache_read_price_per_1m"),
            Some(&json!(0.25))
        );
    }
}
