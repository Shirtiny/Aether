use super::{
    build_admin_payments_backend_unavailable_response, build_admin_payments_bad_request_response,
};
use crate::handlers::admin::request::{AdminAppState, AdminRequestContext};
use crate::{GatewayError, LocalMutationOutcome};
use aether_data_contracts::repository::billing::PaymentGatewayConfigWriteInput;
use axum::{
    body::Body,
    http,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct EpayGatewayConfigRequest {
    #[serde(default)]
    enabled: bool,
    endpoint_url: String,
    #[serde(default)]
    callback_base_url: Option<String>,
    merchant_id: String,
    #[serde(default)]
    merchant_key: Option<String>,
    #[serde(default = "default_pay_currency")]
    pay_currency: String,
    #[serde(default = "default_usd_exchange_rate")]
    usd_exchange_rate: f64,
    #[serde(default = "default_min_recharge_usd")]
    min_recharge_usd: f64,
    #[serde(default = "default_channels")]
    channels: serde_json::Value,
}

fn default_pay_currency() -> String {
    "CNY".to_string()
}

fn default_usd_exchange_rate() -> f64 {
    7.2
}

fn default_min_recharge_usd() -> f64 {
    1.0
}

fn default_channels() -> serde_json::Value {
    json!([
        {"channel": "alipay", "display_name": "支付宝"},
        {"channel": "wxpay", "display_name": "微信支付"}
    ])
}

fn normalize_text(value: impl Into<String>, field: &str, max_len: usize) -> Result<String, String> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if trimmed.chars().count() > max_len {
        return Err(format!("{field} exceeds maximum length {max_len}"));
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_text(
    value: Option<String>,
    max_len: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > max_len {
        return Err(format!("field exceeds maximum length {max_len}"));
    }
    Ok(Some(trimmed.to_string()))
}

fn gateway_config_payload(
    record: aether_data_contracts::repository::billing::PaymentGatewayConfigRecord,
) -> serde_json::Value {
    json!({
        "provider": record.provider,
        "enabled": record.enabled,
        "endpoint_url": record.endpoint_url,
        "callback_base_url": record.callback_base_url,
        "merchant_id": record.merchant_id,
        "has_secret": record.merchant_key_encrypted.as_deref().is_some_and(|value| !value.trim().is_empty()),
        "pay_currency": record.pay_currency,
        "usd_exchange_rate": record.usd_exchange_rate,
        "min_recharge_usd": record.min_recharge_usd,
        "channels": record.channels_json,
        "created_at": record.created_at_unix_secs,
        "updated_at": record.updated_at_unix_secs,
    })
}

pub(super) async fn maybe_build_local_admin_payment_gateways_response(
    state: &AdminAppState<'_>,
    _request_context: &AdminRequestContext<'_>,
    request_body: Option<&axum::body::Bytes>,
    route_kind: Option<&str>,
) -> Result<Option<Response<Body>>, GatewayError> {
    match route_kind {
        Some("get_epay_gateway") => {
            let record = state.app().find_payment_gateway_config("epay").await?;
            let payload = record.map(gateway_config_payload).unwrap_or_else(
                || json!({"provider": "epay", "enabled": false, "has_secret": false}),
            );
            Ok(Some(Json(payload).into_response()))
        }
        Some("update_epay_gateway") => {
            let Some(body) = request_body else {
                return Ok(Some(build_admin_payments_bad_request_response(
                    "缺少请求体",
                )));
            };
            let payload = match serde_json::from_slice::<EpayGatewayConfigRequest>(body) {
                Ok(value) => value,
                Err(_) => {
                    return Ok(Some(build_admin_payments_bad_request_response(
                        "输入验证失败",
                    )))
                }
            };
            if !payload.usd_exchange_rate.is_finite() || payload.usd_exchange_rate <= 0.0 {
                return Ok(Some(build_admin_payments_bad_request_response(
                    "usd_exchange_rate must be positive",
                )));
            }
            if !payload.min_recharge_usd.is_finite() || payload.min_recharge_usd <= 0.0 {
                return Ok(Some(build_admin_payments_bad_request_response(
                    "min_recharge_usd must be positive",
                )));
            }
            let merchant_key_encrypted = match payload
                .merchant_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(secret) => match state.encrypt_catalog_secret_with_fallbacks(secret) {
                    Some(value) => Some(value),
                    None => {
                        return Ok(Some(build_admin_payments_backend_unavailable_response(
                            "encryption key is not configured",
                        )))
                    }
                },
                None => None,
            };
            let endpoint_url = match normalize_text(payload.endpoint_url, "endpoint_url", 512) {
                Ok(value) => value,
                Err(detail) => return Ok(Some(build_admin_payments_bad_request_response(detail))),
            };
            let callback_base_url = match normalize_optional_text(payload.callback_base_url, 512) {
                Ok(value) => value,
                Err(detail) => return Ok(Some(build_admin_payments_bad_request_response(detail))),
            };
            let merchant_id = match normalize_text(payload.merchant_id, "merchant_id", 128) {
                Ok(value) => value,
                Err(detail) => return Ok(Some(build_admin_payments_bad_request_response(detail))),
            };
            let pay_currency = match normalize_text(payload.pay_currency, "pay_currency", 16) {
                Ok(value) => value,
                Err(detail) => return Ok(Some(build_admin_payments_bad_request_response(detail))),
            };
            let input = PaymentGatewayConfigWriteInput {
                provider: "epay".to_string(),
                enabled: payload.enabled,
                endpoint_url,
                callback_base_url,
                merchant_id,
                preserve_existing_secret: merchant_key_encrypted.is_none(),
                merchant_key_encrypted,
                pay_currency,
                usd_exchange_rate: payload.usd_exchange_rate,
                min_recharge_usd: payload.min_recharge_usd,
                channels_json: payload.channels,
            };
            match state.app().upsert_payment_gateway_config(&input).await? {
                LocalMutationOutcome::Applied(record) => {
                    Ok(Some(Json(gateway_config_payload(record)).into_response()))
                }
                _ => Ok(Some(build_admin_payments_backend_unavailable_response(
                    "payment gateway config backend unavailable",
                ))),
            }
        }
        Some("test_epay_gateway") => {
            let status = state.app().find_payment_gateway_config("epay").await?;
            let ok = status
                .as_ref()
                .is_some_and(|record| record.enabled && record.merchant_key_encrypted.is_some());
            Ok(Some(
                (
                    if ok {
                        http::StatusCode::OK
                    } else {
                        http::StatusCode::BAD_REQUEST
                    },
                    Json(json!({"ok": ok, "provider": "epay"})),
                )
                    .into_response(),
            ))
        }
        _ => Ok(None),
    }
}
