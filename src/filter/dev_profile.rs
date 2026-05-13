//! 过滤条件③④⑤：dev 画像
//!
//!   ③ dev_max_open_count    —— dev 历史毕业 (migrated) token 数 ≤ N
//!   ④ dev_max_created_count —— dev 总创建 token 数 ≤ N
//!   ⑤ dev_max_twitter_bound —— dev Twitter 绑定 token 数 ≤ N
//!
//! 架构：`DevProvider` 是数据源抽象。当前只有 `stub()` 实现（永远返回"无数据"）。
//! 后续接入 GMGN / BullX / 自建索引 时新增一个 `DevProvider::xxx()` 工厂方法
//! 并把内部实现指向真实数据源，**filter::mod / main.rs 无需任何修改**。
//!
//! 数据源选项（D2 待用户拍板）：
//!   - A. GMGN 私有 API：需 reverse engineering endpoint + auth；风险中
//!   - B. BullX 私有 API：同 A
//!   - C. 自建索引：YellowStone 历史回扫 + 实时增量，1-2 周开发
//!   - D. Helius DAS：需调研 creator 维度查询能力
//!
//! 当前 stub 行为：永远 Pass（不阻塞实测）。接入真实 provider 后过滤才生效。

use solana_sdk::pubkey::Pubkey;

use super::FilterOutcome;
use crate::groups::CopyGroup;

/// dev 钱包画像统计
#[derive(Debug, Clone, Copy, Default)]
pub struct DevStats {
    /// 已毕业的 token 数（migrated to PumpSwap/Raydium）
    pub open_count: u32,
    /// dev 总共创建过的 token 数
    pub created_count: u32,
    /// dev 推特账号绑定过的 token 数
    pub twitter_bound: u32,
}

/// dev 数据源抽象。
/// 内部用 enum 而不是 trait object，避免动态分发开销；后续接入真实数据源时
/// 在此 enum 加新 variant 即可。
#[derive(Clone)]
pub enum DevProvider {
    /// 无数据源 - `lookup` 永远返回 None，filter 永远 Pass
    Stub,
    // 未来接入数据源时按需添加 variant：
    // Gmgn(GmgnClient),
    // BullX(BullxClient),
    // LocalIndex(Arc<DevIndex>),
    // Helius(HeliusClient),
}

impl DevProvider {
    /// 占位实现：永远返回"无数据"
    pub fn stub() -> Self {
        Self::Stub
    }

    /// 查询 dev 画像。返回 None 表示数据不可用（filter 应默认 Pass）。
    pub fn lookup(&self, _dev: &Pubkey) -> Option<DevStats> {
        match self {
            Self::Stub => None,
        }
    }
}

pub fn check(group: &CopyGroup, source_wallet: &Pubkey, provider: &DevProvider) -> FilterOutcome {
    let Some(stats) = provider.lookup(source_wallet) else {
        // 数据不可用 → 默认 Pass（避免误锁；等接入真实数据源）
        return FilterOutcome::Pass;
    };

    if let Some(limit) = group.dev_max_open_count {
        if stats.open_count > limit {
            return FilterOutcome::Reject(format!(
                "dev_open={} > limit={}",
                stats.open_count, limit
            ));
        }
    }
    if let Some(limit) = group.dev_max_created_count {
        if stats.created_count > limit {
            return FilterOutcome::Reject(format!(
                "dev_created={} > limit={}",
                stats.created_count, limit
            ));
        }
    }
    if let Some(limit) = group.dev_max_twitter_bound {
        if stats.twitter_bound > limit {
            return FilterOutcome::Reject(format!(
                "dev_tw={} > limit={}",
                stats.twitter_bound, limit
            ));
        }
    }

    FilterOutcome::Pass
}
