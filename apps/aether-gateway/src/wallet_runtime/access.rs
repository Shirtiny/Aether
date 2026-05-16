use aether_data::repository::wallet::StoredWalletSnapshot;
use aether_wallet::{
    WalletAccessDecision, WalletAccessFailure, WalletLimitMode, WalletSnapshot, WalletStatus,
};

use crate::control::GatewayLocalAuthRejection;
use crate::data::auth::GatewayAuthApiKeySnapshot;
use crate::{AppState, GatewayError};

const DAILY_QUOTA_EPSILON_USD: f64 = 0.000_000_01;

pub(crate) async fn resolve_wallet_auth_gate(
    state: &AppState,
    auth_snapshot: &GatewayAuthApiKeySnapshot,
) -> Result<Option<WalletAccessDecision>, GatewayError> {
    if !state.has_wallet_data_reader() {
        return Ok(None);
    }

    let wallet = state
        .read_wallet_snapshot_for_auth(
            &auth_snapshot.user_id,
            &auth_snapshot.api_key_id,
            auth_snapshot.api_key_is_standalone,
        )
        .await?;
    let is_admin = wallet_auth_allows_admin_bypass(
        &auth_snapshot.user_role,
        auth_snapshot.api_key_is_standalone,
    );

    let decision = match wallet.as_ref() {
        Some(wallet) => map_wallet_snapshot(wallet).access_decision(is_admin),
        None if is_admin => WalletAccessDecision::allowed(None),
        None => WalletAccessDecision::wallet_unavailable(None),
    };
    if !auth_snapshot.api_key_is_standalone {
        if let Some(quota) = state
            .find_user_daily_quota_availability(&auth_snapshot.user_id)
            .await?
            .filter(|quota| quota.has_active_daily_quota)
        {
            let has_remaining_quota = quota.remaining_usd > DAILY_QUOTA_EPSILON_USD;
            if decision.failure == Some(WalletAccessFailure::BalanceDenied) && has_remaining_quota {
                return Ok(Some(WalletAccessDecision::allowed(Some(
                    quota.remaining_usd,
                ))));
            }
            if decision.failure.is_none() && !quota.allow_wallet_overage && !has_remaining_quota {
                return Ok(Some(WalletAccessDecision::balance_denied(Some(0.0))));
            }
        }
    }
    Ok(Some(decision))
}

pub(crate) fn local_rejection_from_wallet_access(
    decision: &WalletAccessDecision,
) -> Option<GatewayLocalAuthRejection> {
    match decision.failure.as_ref() {
        Some(WalletAccessFailure::WalletUnavailable) => {
            Some(GatewayLocalAuthRejection::WalletUnavailable)
        }
        Some(WalletAccessFailure::BalanceDenied) => {
            Some(GatewayLocalAuthRejection::BalanceDenied {
                remaining: decision.remaining,
            })
        }
        None => None,
    }
}

fn map_wallet_snapshot(snapshot: &StoredWalletSnapshot) -> WalletSnapshot {
    WalletSnapshot {
        wallet_id: snapshot.id.clone(),
        user_id: snapshot.user_id.clone(),
        api_key_id: snapshot.api_key_id.clone(),
        recharge_balance: snapshot.balance,
        gift_balance: snapshot.gift_balance,
        limit_mode: WalletLimitMode::parse(&snapshot.limit_mode),
        currency: snapshot.currency.clone(),
        status: WalletStatus::parse(&snapshot.status),
    }
}

fn wallet_auth_allows_admin_bypass(user_role: &str, api_key_is_standalone: bool) -> bool {
    user_role.eq_ignore_ascii_case("admin") && !api_key_is_standalone
}

#[cfg(test)]
mod tests {
    use aether_data::repository::wallet::StoredWalletSnapshot;
    use aether_wallet::{WalletAccessFailure, WalletLimitMode, WalletSnapshot, WalletStatus};

    use super::{
        local_rejection_from_wallet_access, map_wallet_snapshot, wallet_auth_allows_admin_bypass,
    };
    use crate::control::GatewayLocalAuthRejection;

    #[test]
    fn maps_wallet_snapshot_and_derives_balance_denied() {
        let stored = StoredWalletSnapshot::new(
            "wallet-1".to_string(),
            Some("user-1".to_string()),
            None,
            0.0,
            0.0,
            "finite".to_string(),
            "USD".to_string(),
            "active".to_string(),
            0.0,
            0.0,
            0.0,
            0.0,
            100,
        )
        .expect("wallet should build");

        let decision = map_wallet_snapshot(&stored).access_decision(false);
        assert_eq!(decision.failure, Some(WalletAccessFailure::BalanceDenied));
        assert_eq!(
            local_rejection_from_wallet_access(&decision),
            Some(GatewayLocalAuthRejection::BalanceDenied {
                remaining: Some(0.0),
            })
        );
    }

    #[test]
    fn unlimited_admin_wallet_gate_allows_without_remaining() {
        let decision = WalletSnapshot {
            wallet_id: "wallet-1".to_string(),
            user_id: Some("user-1".to_string()),
            api_key_id: None,
            recharge_balance: 0.0,
            gift_balance: 0.0,
            limit_mode: WalletLimitMode::Unlimited,
            currency: "USD".to_string(),
            status: WalletStatus::Active,
        }
        .access_decision(true);

        assert!(decision.allowed);
        assert_eq!(decision.remaining, None);
    }

    #[test]
    fn standalone_key_never_uses_admin_wallet_bypass() {
        assert!(wallet_auth_allows_admin_bypass("admin", false));
        assert!(!wallet_auth_allows_admin_bypass("admin", true));
    }
}
