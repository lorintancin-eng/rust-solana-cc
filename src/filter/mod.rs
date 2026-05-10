//! 进场过滤模块（2ev 反向跟单策略）
//!
//! 仅在 `entry_mode == ENTRY_MODE_SMART_SELL`（反向跟单）路径上生效，
//! 不影响传统 SMART_BUY 跟单组的现有行为。
//!
//! 5 条过滤条件：
//!   ① max_entry_mcap_usd —— 入场市值上限（USD）。同步实现，零 RPC。
//!   ② require_social_link —— 至少一个社交链接。【TODO 占位，默认通过】
//!   ③ dev_max_open_count —— dev 历史毕业数上限。【TODO 占位，默认通过】
//!   ④ dev_max_created_count —— dev 总创建数上限。【TODO 占位，默认通过】
//!   ⑤ dev_max_twitter_bound —— dev 推特绑币数上限。【TODO 占位，默认通过】
//!
//! 配置任意一项才会启用对应过滤；全 None/false 时整体相当于关闭，无开销。

mod dev_profile;
mod mcap;
mod social;

use solana_sdk::pubkey::Pubkey;

use crate::groups::CopyGroup;
use crate::grpc::BondingCurveCache;
use crate::utils::sol_price::SolUsdPrice;

/// 单个 trade 是否通过 5 条进场过滤
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOutcome {
    /// 全部启用的过滤项均通过（或未启用任何过滤）
    Pass,
    /// 至少一项拒绝，附原因（用于日志）
    Reject(String),
}

impl FilterOutcome {
    pub fn is_pass(&self) -> bool {
        matches!(self, FilterOutcome::Pass)
    }
}

/// 进场过滤器集合
#[derive(Clone)]
pub struct EntryFilters {
    bc_cache: BondingCurveCache,
    sol_usd: SolUsdPrice,
}

impl EntryFilters {
    pub fn new(bc_cache: BondingCurveCache, sol_usd: SolUsdPrice) -> Self {
        Self { bc_cache, sol_usd }
    }

    /// 评估单个 trade 是否通过 group 配置的全部过滤项。
    /// 任何一项拒绝即整体拒绝。
    /// 数据缺失（如 BondingCurveCache 还没填充）时该项默认通过 —— 抢入路径优先。
    pub fn evaluate(
        &self,
        group: &CopyGroup,
        mint: &Pubkey,
        source_wallet: &Pubkey,
    ) -> FilterOutcome {
        if group.has_mcap_filter() {
            if let FilterOutcome::Reject(reason) =
                mcap::check(group, mint, &self.bc_cache, &self.sol_usd)
            {
                return FilterOutcome::Reject(reason);
            }
        }

        if group.require_social_link {
            if let FilterOutcome::Reject(reason) = social::check(group, mint) {
                return FilterOutcome::Reject(reason);
            }
        }

        if group.has_dev_filter() {
            if let FilterOutcome::Reject(reason) = dev_profile::check(group, source_wallet) {
                return FilterOutcome::Reject(reason);
            }
        }

        FilterOutcome::Pass
    }
}
