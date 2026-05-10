//! 过滤条件②：要求 token 至少有一个社交链接（Twitter / Telegram / Website）
//!
//! 当前为 TODO 占位实现，永远返回 Pass。
//!
//! 接入步骤（未来实施）：
//!   1. 计算 Metaplex Token Metadata PDA：
//!      seeds = [b"metadata", METADATA_PROGRAM_ID.as_ref(), mint.as_ref()]
//!      程序 ID: metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s
//!   2. RPC `get_account_info` 拉取该 PDA
//!   3. borsh 解码（mpl-token-metadata crate 或手写）取出 `data.uri`
//!   4. HTTP GET URI（IPFS gateway 或 Arweave），解析 JSON 中的
//!      `twitter` / `telegram` / `website` 字段，任意非空即通过
//!   5. 用 DashMap<Pubkey, bool> 缓存结果，TTL 24h
//!
//! 性能要求：必须异步、不可阻塞 main.rs 的同区块抢入路径。
//! 推荐方案：
//!   - 触发 trade 时立即 spawn 抓取任务
//!   - 抢入路径同步读缓存：命中用结果，未命中默认通过
//!   - 后续仓位监控周期里复检；不通过则触发主动卖出

use solana_sdk::pubkey::Pubkey;

use super::FilterOutcome;
use crate::groups::CopyGroup;

pub fn check(_group: &CopyGroup, _mint: &Pubkey) -> FilterOutcome {
    // TODO: 实现 Metaplex metadata 抓取 + IPFS JSON 解析
    FilterOutcome::Pass
}
