mod memory;
mod mysql;
mod postgres;
mod sqlite;

const SETTLEMENT_EPSILON_USD: f64 = 0.000_000_01;

#[derive(Debug, Clone, Copy)]
struct WalletDebitPlan {
    recharge_deduction: f64,
    gift_deduction: f64,
}

impl WalletDebitPlan {
    fn covered_usd(self) -> f64 {
        self.recharge_deduction + self.gift_deduction
    }
}

fn finite_wallet_available_usd(recharge_balance: f64, gift_balance: f64) -> f64 {
    recharge_balance.max(0.0) + gift_balance.max(0.0)
}

fn plan_finite_wallet_debit(
    recharge_balance: f64,
    gift_balance: f64,
    requested_usd: f64,
) -> WalletDebitPlan {
    let recharge_deduction = recharge_balance.max(0.0).min(requested_usd.max(0.0));
    let gift_deduction = gift_balance
        .max(0.0)
        .min((requested_usd - recharge_deduction).max(0.0));
    WalletDebitPlan {
        recharge_deduction,
        gift_deduction,
    }
}

#[allow(unused_imports)]
pub(crate) use aether_data_contracts::repository::settlement::{
    SettlementRepository, SettlementWriteRepository, StoredUsageSettlement, UsageSettlementInput,
};
pub use memory::InMemorySettlementRepository;
pub use mysql::MysqlSettlementRepository;
pub use postgres::SqlxSettlementRepository;
pub use sqlite::SqliteSettlementRepository;
