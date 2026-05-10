//! 过滤条件③④⑤：dev 画像
//!
//!   ③ dev_max_open_count    —— dev 历史毕业（migrated）token 数 ≤ N
//!   ④ dev_max_created_count —— dev 总创建 token 数 ≤ N
//!   ⑤ dev_max_twitter_bound —— dev Twitter 绑定 token 数 ≤ N
//!
//! 当前为 TODO 占位实现，永远返回 Pass。
//!
//! 数据源决策（D2，待用户拍板）：
//!   选项 A: GMGN 私有 API
//!     - 速度快，0 工程
//!     - 需 reverse engineering endpoint + auth
//!     - 速率限制 / IP 封禁风险
//!     - schema 可能突变
//!   选项 B: BullX 私有 API（同 A 风险）
//!   选项 C: 自建 dev 索引
//!     - YellowStone gRPC 历史回扫 pump.fun `create` 指令
//!     - 实时增量索引到本地 sled / sqlite
//!     - 1-2 周开发，但完全可控
//!     - 推荐长期方案
//!   选项 D: Helius DAS API
//!     - 公开 API + 付费方案
//!     - 需调研 creator → tokens 索引能力
//!
//! 接入接口（实施时只需实现这一函数）：
//!   pub fn check(group: &CopyGroup, source_wallet: &Pubkey) -> FilterOutcome
//!
//! 缓存建议：DashMap<Pubkey /* dev */, DevStats> + TTL 1h
//!
//! 性能要求：dev 数据通常变化慢（分钟~小时级），可异步预热 + 同步读缓存。

use solana_sdk::pubkey::Pubkey;

use super::FilterOutcome;
use crate::groups::CopyGroup;

pub fn check(_group: &CopyGroup, _source_wallet: &Pubkey) -> FilterOutcome {
    // TODO: 接入 dev 数据源后替换此实现
    // 参考字段：group.dev_max_open_count / dev_max_created_count / dev_max_twitter_bound
    FilterOutcome::Pass
}
