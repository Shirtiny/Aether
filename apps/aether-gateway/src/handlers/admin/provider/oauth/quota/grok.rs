use super::shared::{
    build_provider_quota_execution_plan, build_quota_snapshot_payload, execute_provider_quota_plan,
    extract_execution_error_message, persist_provider_quota_refresh_state,
    quota_refresh_success_invalid_state, resolve_provider_quota_execution_timeouts,
    ProviderQuotaExecutionOutcome,
};
use crate::handlers::admin::provider::pool::runtime::record_admin_provider_pool_grok_auth_cooldown;
use crate::handlers::admin::request::{AdminAppState, AdminGatewayProviderTransportSnapshot};
use crate::GatewayError;
use aether_contracts::{ExecutionResult, ProxySnapshot};
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use aether_provider_pool::{
    build_grok_billing_request, build_grok_pool_quota_request,
    grok_billing_has_authoritative_quota, merge_grok_billing_snapshot, merge_grok_quota_snapshot,
    parse_grok_billing_payload, parse_grok_quota_headers, GrokBillingWindow,
    ProviderPoolQuotaRequestSpec,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

async fn execute_grok_quota_plan(
    state: &AdminAppState<'_>,
    transport: &AdminGatewayProviderTransportSnapshot,
    mut spec: ProviderPoolQuotaRequestSpec,
    proxy_override: Option<&ProxySnapshot>,
) -> Result<ProviderQuotaExecutionOutcome, GatewayError> {
    // The probe only exists to observe rate-limit headers, so it has to look
    // like the chat traffic it is measuring.
    crate::provider_transport::apply_grok_chat_identity_headers(&mut spec.headers, transport);
    let proxy = match proxy_override {
        Some(proxy) => Some(proxy.clone()),
        None => {
            state
                .resolve_transport_proxy_snapshot_with_tunnel_affinity(transport)
                .await
        }
    };
    let timeouts = Some(resolve_provider_quota_execution_timeouts(
        state.resolve_transport_execution_timeouts(transport),
        proxy.as_ref(),
    ));
    let plan = build_provider_quota_execution_plan(
        transport,
        spec,
        proxy,
        state.resolve_transport_profile(transport),
        timeouts,
    );
    execute_provider_quota_plan(state, transport, plan, "grok:xai_headers").await
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn grok_result_error_detail(result: &ExecutionResult) -> String {
    extract_execution_error_message(result)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("xAI quota probe returned HTTP {}", result.status_code))
}

fn grok_metadata_update(snapshot: Value) -> Value {
    json!({ "grok": snapshot })
}

fn grok_quota_probe_succeeded(status_code: u16, quota_snapshot: Option<&Value>) -> bool {
    (200..300).contains(&status_code) && quota_snapshot.is_some()
}

struct GrokBillingWindowOutcome {
    parsed: Option<Value>,
    status_code: u16,
}

/// Status code that best describes a billing pair: a window that answered
/// successfully outranks one that failed, and a 403 outranks other failures
/// because it is the account-level verdict rather than a transport hiccup.
fn effective_grok_billing_status(weekly: u16, monthly: u16) -> u16 {
    if let Some(ok) = [weekly, monthly]
        .into_iter()
        .find(|code| (200..300).contains(code))
    {
        return ok;
    }
    if weekly == 403 || monthly == 403 {
        return 403;
    }
    if weekly != 0 {
        weekly
    } else {
        monthly
    }
}

/// Fetch one xAI billing window. A window that cannot be reached reports status
/// `0` so the merge keeps whatever it previously knew about that window.
async fn fetch_grok_billing_window(
    state: &AdminAppState<'_>,
    transport: &AdminGatewayProviderTransportSnapshot,
    authorization: Option<(String, String)>,
    proxy: Option<&ProxySnapshot>,
    window: GrokBillingWindow,
    observed_at: u64,
) -> Result<GrokBillingWindowOutcome, GatewayError> {
    let unreachable = GrokBillingWindowOutcome {
        parsed: None,
        status_code: 0,
    };
    let spec = match build_grok_billing_request(
        &transport.key.id,
        &transport.endpoint.base_url,
        authorization,
        Some(transport.key.decrypted_api_key.as_str()),
        window,
    ) {
        Ok(spec) => spec,
        Err(_) => return Ok(unreachable),
    };
    let result = match execute_grok_quota_plan(state, transport, spec, proxy).await? {
        ProviderQuotaExecutionOutcome::Response(result) => result,
        ProviderQuotaExecutionOutcome::Failure(_) => return Ok(unreachable),
    };
    let parsed = (200..300)
        .contains(&result.status_code)
        .then(|| {
            result
                .body
                .as_ref()
                .and_then(|body| body.json_body.as_ref())
                .and_then(|body| parse_grok_billing_payload(body, observed_at))
        })
        .flatten();
    Ok(GrokBillingWindowOutcome {
        parsed,
        status_code: result.status_code,
    })
}

pub(crate) async fn refresh_grok_provider_quota_locally(
    state: &AdminAppState<'_>,
    provider: &StoredProviderCatalogProvider,
    endpoint: &StoredProviderCatalogEndpoint,
    keys: Vec<StoredProviderCatalogKey>,
    proxy_override: Option<ProxySnapshot>,
) -> Result<Option<Value>, GatewayError> {
    let mut results = Vec::new();
    let mut success_count = 0usize;
    let mut failed_count = 0usize;

    for key in keys {
        let transport = match state
            .read_provider_transport_snapshot(&provider.id, &endpoint.id, &key.id)
            .await?
        {
            Some(transport) => transport,
            None => {
                failed_count += 1;
                results.push(json!({
                    "key_id": key.id,
                    "key_name": key.name,
                    "status": "error",
                    "message": "Provider transport snapshot unavailable",
                }));
                continue;
            }
        };

        let authorization = match state.resolve_local_oauth_header_auth(&transport).await? {
            Some(auth) => auth,
            None => {
                failed_count += 1;
                results.push(json!({
                    "key_id": key.id,
                    "key_name": key.name,
                    "status": "error",
                    "message": "缺少 xAI OAuth 认证信息，请先授权/刷新 Token",
                }));
                continue;
            }
        };

        // Keep every lookup for this key on one tunnel, so the billing pair and
        // any follow-up probe describe the same egress path.
        let proxy = match proxy_override.clone() {
            Some(proxy) => Some(proxy),
            None => {
                state
                    .resolve_transport_proxy_snapshot_with_tunnel_affinity(&transport)
                    .await
            }
        };
        let observed_at = now_unix_secs();

        // Billing is the only signal that describes the subscription allowance:
        // a paid account's rate-limit headers are a static ceiling. It is also a
        // plain GET, so refreshing a paid key costs no model quota.
        let (weekly, monthly) = tokio::join!(
            fetch_grok_billing_window(
                state,
                &transport,
                Some(authorization.clone()),
                proxy.as_ref(),
                GrokBillingWindow::Weekly,
                observed_at,
            ),
            fetch_grok_billing_window(
                state,
                &transport,
                Some(authorization.clone()),
                proxy.as_ref(),
                GrokBillingWindow::Monthly,
                observed_at,
            ),
        );
        let (weekly, monthly) = (weekly?, monthly?);
        let previous_grok = key
            .upstream_metadata
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|metadata| metadata.get("grok"));
        let billing = merge_grok_billing_snapshot(
            previous_grok.and_then(|bucket| bucket.get("billing")),
            weekly.parsed.as_ref(),
            monthly.parsed.as_ref(),
            weekly.status_code,
            monthly.status_code,
            observed_at,
        );
        // The merge deliberately keeps a window's previous value when its lookup
        // fails, so authority additionally requires a fresh observation: a total
        // billing outage must fall back to the probe rather than re-report a
        // stale allowance as if it had just been confirmed.
        let billing_authoritative = (weekly.parsed.is_some() || monthly.parsed.is_some())
            && grok_billing_has_authoritative_quota(billing.as_ref());

        // Free accounts answer billing successfully but say nothing about their
        // allowance, so only they still pay for the rate-limit header probe.
        let result = if billing_authoritative {
            None
        } else {
            let spec = match build_grok_pool_quota_request(
                &transport.key.id,
                &transport.endpoint.base_url,
                Some(authorization),
                Some(transport.key.decrypted_api_key.as_str()),
            ) {
                Ok(spec) => spec,
                Err(message) => {
                    failed_count += 1;
                    results.push(json!({
                        "key_id": key.id,
                        "key_name": key.name,
                        "status": "error",
                        "message": message,
                    }));
                    continue;
                }
            };
            match execute_grok_quota_plan(state, &transport, spec, proxy.as_ref()).await? {
                ProviderQuotaExecutionOutcome::Response(result) => Some(result),
                ProviderQuotaExecutionOutcome::Failure(detail) => {
                    failed_count += 1;
                    results.push(json!({
                        "key_id": key.id,
                        "key_name": key.name,
                        "status": "error",
                        "message": format!("xAI quota probe 执行失败: {detail}"),
                    }));
                    continue;
                }
            }
        };

        let status_code = match result.as_ref() {
            Some(result) => result.status_code,
            None => effective_grok_billing_status(weekly.status_code, monthly.status_code),
        };
        let mut incoming = result.as_ref().and_then(|result| {
            parse_grok_quota_headers(&result.headers, result.status_code, observed_at)
        });
        if let Some(billing) = billing.as_ref() {
            incoming = Some(match incoming {
                Some(mut snapshot) => {
                    if let Some(object) = snapshot.as_object_mut() {
                        object.insert("billing".to_string(), billing.clone());
                    }
                    snapshot
                }
                // Billing alone must not restamp the header observation: this
                // lookup did not re-observe the rate-limit windows.
                None => json!({
                    "version": 2,
                    "provider_type": "grok",
                    "billing": billing,
                }),
            });
        }
        let quota_snapshot =
            incoming.map(|snapshot| merge_grok_quota_snapshot(previous_grok, &snapshot));
        let metadata_update = quota_snapshot.clone().map(grok_metadata_update);
        let error_detail = (!(200..300).contains(&status_code)).then(|| match result.as_ref() {
            Some(result) => grok_result_error_detail(result),
            None => format!("xAI billing 查询返回 HTTP {status_code}"),
        });
        let _ = record_admin_provider_pool_grok_auth_cooldown(
            state.app().runtime_state.as_ref(),
            &provider.id,
            &key.id,
            status_code,
        )
        .await;
        let (invalid_at, invalid_reason) = if (200..300).contains(&status_code) {
            quota_refresh_success_invalid_state(&key)
        } else {
            (
                key.oauth_invalid_at_unix_secs,
                key.oauth_invalid_reason.clone(),
            )
        };

        if !persist_provider_quota_refresh_state(
            state,
            &key.id,
            metadata_update.as_ref(),
            invalid_at,
            invalid_reason,
            None,
        )
        .await?
        {
            failed_count += 1;
            results.push(json!({
                "key_id": key.id,
                "key_name": key.name,
                "status": "error",
                "message": "Key 状态写入失败",
            }));
            continue;
        }

        let probe_succeeded = grok_quota_probe_succeeded(status_code, quota_snapshot.as_ref());
        if probe_succeeded {
            success_count += 1;
        } else {
            failed_count += 1;
        }

        let mut payload = serde_json::Map::from_iter([
            ("key_id".to_string(), json!(key.id)),
            ("key_name".to_string(), json!(key.name)),
            (
                "status".to_string(),
                json!(if probe_succeeded { "success" } else { "error" }),
            ),
            ("status_code".to_string(), json!(status_code)),
        ]);
        if let Some(snapshot) = quota_snapshot {
            payload.insert("metadata".to_string(), snapshot);
        }
        if let Some(snapshot) = build_quota_snapshot_payload(
            "grok",
            key.status_snapshot.as_ref(),
            metadata_update.as_ref(),
        ) {
            payload.insert("quota_snapshot".to_string(), snapshot);
        }
        if let Some(message) = error_detail {
            payload.insert("message".to_string(), json!(message));
        } else if !probe_succeeded {
            payload.insert(
                "message".to_string(),
                json!("xAI 响应未包含可识别的 x-ratelimit-* 配额头"),
            );
        }
        results.push(Value::Object(payload));
    }

    Ok(Some(json!({
        "success": success_count,
        "failed": failed_count,
        "total": success_count + failed_count,
        "results": results,
        "message": format!("已处理 {} 个 Key", success_count + failed_count),
        "auto_removed": 0,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_xai_snapshot_in_grok_metadata_bucket() {
        let update = grok_metadata_update(json!({
            "provider_type": "grok",
            "windows": [{"code": "requests", "remaining": 1}],
        }));
        assert_eq!(update["grok"]["provider_type"], "grok");
        assert_eq!(update["grok"]["windows"][0]["code"], "requests");
    }

    #[test]
    fn only_successful_xai_responses_count_as_successful_probes() {
        let snapshot = json!({"provider_type": "grok"});
        assert!(grok_quota_probe_succeeded(200, Some(&snapshot)));
        assert!(!grok_quota_probe_succeeded(200, None));
        assert!(!grok_quota_probe_succeeded(429, Some(&snapshot)));
        assert!(!grok_quota_probe_succeeded(500, Some(&snapshot)));
    }
}
