mod capability;
mod plan;
mod presets;
mod provider;
mod quota;
mod quota_refresh;
mod service;

pub mod providers;

pub use capability::{ProviderPoolCapabilities, ProviderPoolCapability};
pub use plan::{derive_oauth_plan_type, derive_plan_tier, normalize_provider_plan_tier};
pub use presets::{
    build_admin_pool_scheduling_presets_payload, normalize_provider_scheduling_presets,
};
pub use provider::{ProviderPoolAdapter, ProviderPoolMemberInput};
pub use providers::{
    build_antigravity_pool_quota_request, build_chatgpt_web_pool_quota_request,
    build_codex_pool_quota_request, build_kiro_pool_quota_request,
    enrich_chatgpt_web_quota_metadata, normalize_chatgpt_web_image_quota_limit,
    AntigravityProviderPoolAdapter, ChatGptWebProviderPoolAdapter, CodexProviderPoolAdapter,
    DefaultProviderPoolAdapter, KiroPoolQuotaAuthInput, KiroProviderPoolAdapter,
    UnsupportedQuotaProviderPoolAdapter, ANTIGRAVITY_FETCH_AVAILABLE_MODELS_PATH,
    CHATGPT_WEB_CONVERSATION_INIT_PATH, CHATGPT_WEB_DEFAULT_BASE_URL, CODEX_WHAM_USAGE_URL,
    KIRO_USAGE_LIMITS_PATH, KIRO_USAGE_SDK_VERSION,
};
pub use quota::{
    provider_pool_key_account_quota_exhausted, provider_pool_key_scheduling_label,
    provider_pool_member_quota_snapshot, provider_pool_quota_metadata_provider_type,
    provider_pool_quota_metadata_updated_at, provider_pool_quota_snapshot_updated_at,
};
pub use quota_refresh::ProviderPoolQuotaRequestSpec;
pub use service::ProviderPoolService;

#[cfg(test)]
mod tests {
    use super::*;
    use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey;
    use aether_pool_core::PoolSchedulingPreset;
    use serde_json::{json, Value};

    fn sample_key(upstream_metadata: Option<Value>) -> StoredProviderCatalogKey {
        let mut key = StoredProviderCatalogKey::new(
            "key-1".to_string(),
            "provider-1".to_string(),
            "key-1".to_string(),
            "oauth".to_string(),
            None,
            true,
        )
        .expect("key should build");
        key.upstream_metadata = upstream_metadata;
        key
    }

    #[test]
    fn builtin_service_registers_provider_pool_adapters() {
        let service = ProviderPoolService::with_builtin_adapters();

        assert_eq!(
            service.provider_types().collect::<Vec<_>>(),
            [
                "antigravity",
                "chatgpt_web",
                "claude_code",
                "codex",
                "gemini_cli",
                "kiro",
                "vertex_ai"
            ]
        );
        assert!(service
            .adapter("codex")
            .capabilities()
            .supports(ProviderPoolCapability::PlanTier));
        assert_eq!(service.adapter("unknown").provider_type(), "default");
    }

    #[test]
    fn builtin_service_owns_quota_refresh_support_and_endpoint_selection() {
        let service = ProviderPoolService::with_builtin_adapters();

        assert_eq!(
            service.provider_types_for_capability(ProviderPoolCapability::QuotaRefresh),
            ["antigravity", "chatgpt_web", "codex", "kiro"]
        );
        assert!(service.supports_quota_refresh("codex"));
        assert!(service.supports_quota_refresh("antigravity"));
        assert!(!service.supports_quota_refresh("gemini_cli"));
        assert!(service.supports_account_self_check("codex"));
        assert!(!service.supports_account_self_check("gemini_cli"));
        assert_eq!(
            service.quota_refresh_unsupported_message("claude_code"),
            "Claude Code 暂不支持自动刷新额度：上游没有稳定可用的账号额度查询接口"
        );
        assert_eq!(
            service.quota_refresh_unsupported_message("vertex_ai"),
            "Vertex AI 暂不支持自动刷新额度：额度属于 Google Cloud 项目/区域配额"
        );
    }

    #[test]
    fn codex_quota_request_adds_account_header_for_paid_accounts() {
        let spec = build_codex_pool_quota_request(
            "key-1",
            Some(("authorization".to_string(), "Bearer access".to_string())),
            None,
            Some(&json!({
                "plan_type": "plus",
                "account_id": "acct-1"
            })),
        )
        .expect("spec should build");

        assert_eq!(
            spec.headers.get("chatgpt-account-id").map(String::as_str),
            Some("acct-1")
        );
    }

    #[test]
    fn codex_quota_request_skips_account_header_for_free_accounts() {
        let spec = build_codex_pool_quota_request(
            "key-1",
            Some(("authorization".to_string(), "Bearer access".to_string())),
            None,
            Some(&json!({
                "plan_type": "codex:free",
                "account_id": "acct-1"
            })),
        )
        .expect("spec should build");

        assert!(!spec.headers.contains_key("chatgpt-account-id"));
    }

    #[test]
    fn kiro_quota_request_includes_profile_arn_when_present() {
        let spec = build_kiro_pool_quota_request(
            "key-1",
            &KiroPoolQuotaAuthInput {
                authorization_value: "Bearer access".to_string(),
                api_region: "us-west-2".to_string(),
                kiro_version: "0.3.210".to_string(),
                machine_id: "machine".to_string(),
                profile_arn: Some("arn:aws:sso:::profile/p-1".to_string()),
            },
        );

        assert!(spec.url.contains("q.us-west-2.amazonaws.com"));
        assert!(spec
            .url
            .contains("profileArn=arn%3Aaws%3Asso%3A%3A%3Aprofile%2Fp-1"));
    }

    #[test]
    fn chatgpt_web_quota_request_uses_default_base_url_when_empty() {
        let spec = build_chatgpt_web_pool_quota_request(
            "key-1",
            "",
            ("authorization".to_string(), "Bearer access".to_string()),
        );

        assert_eq!(
            spec.url,
            "https://chatgpt.com/backend-api/conversation/init"
        );
        assert_eq!(
            spec.headers.get("origin").map(String::as_str),
            Some("https://chatgpt.com")
        );
        assert!(spec.accept_invalid_certs);
    }

    #[test]
    fn chatgpt_web_quota_metadata_enriches_auth_and_normalizes_free_limit() {
        let mut metadata = json!({
            "image_quota_remaining": 12,
        });
        enrich_chatgpt_web_quota_metadata(
            &mut metadata,
            Some(&json!({
                "plan": "free",
                "email": "user@example.com",
                "accountId": "acct-1"
            })),
        );
        normalize_chatgpt_web_image_quota_limit(&mut metadata, None);

        assert_eq!(metadata["plan_type"], json!("free"));
        assert_eq!(metadata["email"], json!("user@example.com"));
        assert_eq!(metadata["account_id"], json!("acct-1"));
        assert_eq!(metadata["image_quota_total"], json!(25.0));
        assert_eq!(metadata["image_quota_used"], json!(13.0));
    }

    #[test]
    fn chatgpt_web_quota_metadata_preserves_existing_paid_limit() {
        let mut metadata = json!({
            "plan_type": "plus",
            "image_quota_remaining": 7,
        });
        normalize_chatgpt_web_image_quota_limit(
            &mut metadata,
            Some(&json!({
                "chatgpt_web": {
                    "image_quota_total": 40
                }
            })),
        );

        assert_eq!(metadata["image_quota_total"], json!(40.0));
        assert_eq!(metadata["image_quota_used"], json!(33.0));
    }

    #[test]
    fn preset_payload_derives_provider_support_from_capabilities() {
        let payload = build_admin_pool_scheduling_presets_payload();
        let items = payload.as_array().expect("payload should be array");
        let free_first = items
            .iter()
            .find(|item| item["name"] == "free_first")
            .expect("free_first should exist");
        let recent_refresh = items
            .iter()
            .find(|item| item["name"] == "recent_refresh")
            .expect("recent_refresh should exist");

        assert_eq!(free_first["providers"], json!(["codex", "kiro"]));
        assert_eq!(recent_refresh["providers"], json!(["codex", "kiro"]));
    }

    #[test]
    fn quota_metadata_provider_type_comes_from_pool_registry() {
        assert_eq!(
            provider_pool_quota_metadata_provider_type(&json!({
                "gemini_cli": {
                    "updated_at": 1_700_000_000u64
                }
            }))
            .as_deref(),
            Some("gemini_cli")
        );
        assert_eq!(
            provider_pool_quota_metadata_provider_type(&json!({
                "custom_provider": {
                    "updated_at": 1_700_000_000u64
                }
            }))
            .as_deref(),
            Some("custom_provider")
        );
    }

    #[test]
    fn codex_adapter_injects_recent_refresh_and_filters_by_capability() {
        let service = ProviderPoolService::with_builtin_adapters();
        let normalized = service.normalize_scheduling_presets(
            "codex",
            &[PoolSchedulingPreset {
                preset: "cache_affinity".to_string(),
                enabled: true,
                mode: None,
            }],
        );

        assert_eq!(
            normalized
                .iter()
                .map(|preset| preset.preset.as_str())
                .collect::<Vec<_>>(),
            ["cache_affinity", "recent_refresh"]
        );

        let unsupported = service.normalize_scheduling_presets(
            "chatgpt_web",
            &[PoolSchedulingPreset {
                preset: "plus_first".to_string(),
                enabled: true,
                mode: None,
            }],
        );
        assert!(unsupported.is_empty());
    }

    #[test]
    fn provider_quota_exhaustion_is_adapter_owned() {
        assert!(provider_pool_key_account_quota_exhausted(
            &sample_key(Some(json!({
                "codex": {
                    "has_credits": false,
                    "credits_unlimited": false
                }
            }))),
            "codex",
        ));
        assert!(provider_pool_key_account_quota_exhausted(
            &sample_key(Some(json!({
                "kiro": {
                    "remaining": 0
                }
            }))),
            "kiro",
        ));
        assert!(provider_pool_key_account_quota_exhausted(
            &sample_key(Some(json!({
                "chatgpt_web": {
                    "image_quota_blocked": true
                }
            }))),
            "chatgpt_web",
        ));
        assert!(!provider_pool_key_account_quota_exhausted(
            &sample_key(Some(json!({
                "codex": {
                    "has_credits": false,
                    "credits_unlimited": true
                }
            }))),
            "codex",
        ));
    }

    #[test]
    fn plan_tier_derivation_normalizes_provider_prefix() {
        let key = sample_key(Some(json!({
            "codex": {
                "plan_type": "codex:Plus"
            }
        })));

        assert_eq!(
            derive_oauth_plan_type("codex", &key, None).as_deref(),
            Some("plus")
        );
    }

    #[test]
    fn plan_tier_derivation_reads_quota_snapshot() {
        let mut key = sample_key(None);
        key.status_snapshot = Some(json!({
            "quota": {
                "plan_type": "team"
            }
        }));

        assert_eq!(
            derive_oauth_plan_type("codex", &key, None).as_deref(),
            Some("team")
        );
    }
}
