use solana_sdk::pubkey::Pubkey;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::groups::CopyGroup;

// ============================================
// 浠撲綅鐘舵€佹満
// Pending 鈫?Submitted 鈫?Confirming 鈫?Active 鈫?Selling 鈫?Closed
//    鈹?         鈹?          鈹?                    鈹?//    鈹斺攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹粹攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹粹攢鈹€鈹€ Failed 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹?// ============================================

/// 浠撲綅鐘舵€?#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionState {
    /// 鍏辫瘑瑙﹀彂锛屼氦鏄撳凡鏋勫缓浣嗘湭鍙戦€?    Pending,
    /// 浜ゆ槗宸插彂閫佸埌鑷冲皯涓€涓€氶亾
    Submitted,
    /// 绛夊緟閾句笂纭锛堟鎹熷凡鐢熸晥锛?    Confirming,
    /// 纭鎴愬姛锛屽畬鏁村姛鑳藉惎鐢紙姝㈢泩+姝㈡崯+杩借釜姝㈡崯锛?    Active,
    /// 姝ｅ湪鎵ц鍗栧嚭
    Selling,
    /// 宸插叧闂?    Closed,
    /// 澶辫触锛堜换浣曢樁娈碉級
    Failed,
}

impl std::fmt::Display for PositionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PositionState::Pending => write!(f, "PENDING"),
            PositionState::Submitted => write!(f, "SUBMITTED"),
            PositionState::Confirming => write!(f, "CONFIRMING"),
            PositionState::Active => write!(f, "ACTIVE"),
            PositionState::Selling => write!(f, "SELLING"),
            PositionState::Closed => write!(f, "CLOSED"),
            PositionState::Failed => write!(f, "FAILED"),
        }
    }
}

/// 浠撲綅淇℃伅锛堝甫鐘舵€佹満锛?#[derive(Debug, Clone)]
pub struct SellAccountSnapshot {
    pub bonding_curve: Pubkey,
    pub associated_bonding_curve: Pubkey,
    pub user_ata: Pubkey,
    pub token_program: Pubkey,
    pub mirror_accounts: Vec<Pubkey>,
    pub source_wallet: Pubkey,
}

#[derive(Debug, Clone)]
pub struct Position {
    pub group: CopyGroup,
    pub token_mint: Pubkey,
    pub state: PositionState,

    // 涔板叆淇℃伅
    pub entry_sol_amount: u64,
    pub token_amount: u64,
    pub entry_price_sol: f64,

    // 浠ｅ竵淇℃伅锛堢‘璁ゅ悗濉厖锛?    pub token_name: String,
    pub entry_mcap_sol: f64,

    // 杩借釜姝㈡崯
    pub highest_price: f64,
    pub current_price: f64,

    // 鏃堕棿
    pub created_at: Instant,
    pub confirmed_at: Option<Instant>,

    // 鏉ユ簮
    pub source_wallet: Pubkey,
    pub buy_signature: String,
    pub sell_signature: Option<String>,
    pub sell_snapshot: Option<SellAccountSnapshot>,
    pub pre_buy_ata_balance: u64,

    // 閲嶈瘯璁℃暟
    pub sell_attempts: u32,
    pub zero_balance_sell_skips: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PositionKey {
    pub group_id: String,
    pub token_mint: Pubkey,
}

impl Position {
    /// 鍒涘缓鏂颁粨浣嶏紙Pending 鐘舵€侊級
    pub fn new(
        group: CopyGroup,
        token_mint: Pubkey,
        entry_sol_amount: u64,
        entry_price_sol: f64,
        source_wallet: Pubkey,
        pre_buy_ata_balance: u64,
    ) -> Self {
        Self {
            group,
            token_mint,
            state: PositionState::Pending,
            entry_sol_amount,
            token_amount: 0,
            entry_price_sol,
            token_name: String::new(),
            entry_mcap_sol: 0.0,
            highest_price: entry_price_sol,
            current_price: entry_price_sol,
            created_at: Instant::now(),
            confirmed_at: None,
            source_wallet,
            buy_signature: String::new(),
            sell_signature: None,
            sell_snapshot: None,
            pre_buy_ata_balance,
            sell_attempts: 0,
            zero_balance_sell_skips: 0,
        }
    }

    pub fn key(&self) -> PositionKey {
        PositionKey {
            group_id: self.group.id.clone(),
            token_mint: self.token_mint,
        }
    }

    pub fn set_sell_snapshot(&mut self, snapshot: SellAccountSnapshot) {
        self.sell_snapshot = Some(snapshot);
    }

    pub fn set_token_amount_estimate(&mut self, token_amount: u64) {
        if self.token_amount == 0 && token_amount > 0 {
            self.token_amount = token_amount;
        }
    }

    // ============================================
    // 鐘舵€佽浆鎹㈡柟娉?    // ============================================

    /// Pending 鈫?Submitted: 浜ゆ槗宸插彂閫?    pub fn mark_submitted(&mut self, signature: String) -> bool {
        if self.state != PositionState::Pending {
            warn!(
                "Cannot mark submitted: {} is in {} state",
                &self.token_mint.to_string()[..12],
                self.state,
            );
            return false;
        }
        self.buy_signature = signature;
        self.state = PositionState::Submitted;
        info!(
            "Position {} 鈫?SUBMITTED | sig: {}",
            &self.token_mint.to_string()[..12],
            &self.buy_signature[..12.min(self.buy_signature.len())],
        );
        true
    }

    /// Submitted 鈫?Confirming: 寮€濮嬬瓑寰呴摼涓婄‘璁わ紙姝㈡崯绔嬪嵆鐢熸晥锛?    pub fn mark_confirming(&mut self) -> bool {
        if self.state != PositionState::Submitted {
            return false;
        }
        self.state = PositionState::Confirming;
        debug!(
            "Position {} 鈫?CONFIRMING (stop-loss active)",
            &self.token_mint.to_string()[..12],
        );
        true
    }

    /// Confirming 鈫?Active: 閾句笂纭鎴愬姛锛屼慨姝ｇ湡瀹炰环鏍?    /// actual_token_amount: raw token amount (鍚?6 浣嶅皬鏁扮簿搴?
    /// bc_price_sol: bonding curve 鐜拌揣浠锋牸 (SOL/token)锛岀敤浣滄垚鏈环
    ///   濡傛灉涓?0 鎴?None锛屽垯鍥為€€鍒?sol_spent / tokens 璁＄畻
    pub fn mark_active(&mut self, actual_token_amount: u64, bc_price_sol: Option<f64>) -> bool {
        if self.state != PositionState::Confirming && self.state != PositionState::Submitted {
            return false;
        }
        self.token_amount = actual_token_amount;
        if actual_token_amount > 0 {
            let display_tokens = actual_token_amount as f64 / 1e6;

            // 浼樺厛鐢?bonding curve 鐜拌揣浠锋牸浣滀负鎴愭湰浠?            // 鍥為€€: sol_spent / tokens锛堝寘鍚?ATA 绉熼噾绛夛紝涓嶅鍑嗙‘锛?            self.entry_price_sol = match bc_price_sol {
                Some(p) if p > 0.0 => p,
                _ if self.entry_sol_amount > 0 => {
                    (self.entry_sol_amount as f64 / 1e9) / display_tokens
                }
                _ => 0.0,
            };

            self.highest_price = self.entry_price_sol;
            self.current_price = self.entry_price_sol;
        }
        self.state = PositionState::Active;
        self.confirmed_at = Some(Instant::now());
        info!(
            "Position {} 鈫?ACTIVE | {:.0} tokens | entry price: {:.10} SOL",
            &self.token_mint.to_string()[..12],
            actual_token_amount as f64 / 1e6,
            self.entry_price_sol,
        );
        true
    }

    /// Active/Confirming 鈫?Selling: 寮€濮嬪崠鍑?    pub fn mark_selling(&mut self) -> bool {
        if self.state != PositionState::Active
            && self.state != PositionState::Confirming
            && self.state != PositionState::Submitted
        {
            return false;
        }
        self.state = PositionState::Selling;
        self.sell_attempts += 1;
        self.zero_balance_sell_skips = 0;
        debug!(
            "Position {} 鈫?SELLING (attempt #{})",
            &self.token_mint.to_string()[..12],
            self.sell_attempts,
        );
        true
    }

    /// Selling 鈫?Closed: 鍗栧嚭鎴愬姛
    pub fn mark_closed(&mut self, sell_signature: String) -> bool {
        if self.state != PositionState::Selling {
            return false;
        }
        self.sell_signature = Some(sell_signature);
        self.state = PositionState::Closed;
        let pnl = self.pnl_percent();
        info!(
            "Position {} 鈫?CLOSED | PnL: {:.2}% | attempts: {}",
            &self.token_mint.to_string()[..12],
            pnl,
            self.sell_attempts,
        );
        true
    }

    /// 浠讳綍鐘舵€?鈫?Failed
    pub fn mark_failed(&mut self, reason: &str) -> bool {
        let old_state = self.state;
        self.state = PositionState::Failed;
        warn!(
            "Position {} 鈫?FAILED (was {}) | reason: {}",
            &self.token_mint.to_string()[..12],
            old_state,
            reason,
        );
        true
    }

    /// Selling 鈫?Active: 鍗栧嚭澶辫触锛屽洖閫€鍒?Active锛堝厑璁搁噸璇曪級
    pub fn revert_to_active(&mut self) -> bool {
        if self.state != PositionState::Selling {
            return false;
        }
        self.state = PositionState::Active;
        debug!(
            "Position {} reverted to ACTIVE (sell attempt #{} failed)",
            &self.token_mint.to_string()[..12],
            self.sell_attempts,
        );
        true
    }

    pub fn restore_after_sell_attempt(&mut self, previous_state: PositionState) -> bool {
        self.state = previous_state;
        true
    }

    pub fn suspend_auto_sell(&mut self, previous_state: PositionState, max_attempts: u32) -> bool {
        self.state = previous_state;
        self.sell_attempts = self.sell_attempts.max(max_attempts);
        warn!(
            "Position {} auto-sell suspended after repeated failures | attempts={}",
            &self.token_mint.to_string()[..12],
            self.sell_attempts,
        );
        true
    }

    pub fn record_zero_balance_sell_skip(&mut self) -> u32 {
        self.zero_balance_sell_skips = self.zero_balance_sell_skips.saturating_add(1);
        self.zero_balance_sell_skips
    }

    pub fn apply_partial_sell(&mut self, sold_amount: u64) -> bool {
        if sold_amount == 0 || sold_amount > self.token_amount {
            return false;
        }
        self.token_amount = self.token_amount.saturating_sub(sold_amount);
        self.state = PositionState::Active;
        true
    }

    // ============================================
    // 浠锋牸鏇存柊鍜?PnL 璁＄畻
    // ============================================

    /// 鏇存柊褰撳墠浠锋牸锛岃繑鍥炴槸鍚﹀垱鏂伴珮
    pub fn update_price(&mut self, price: f64) -> bool {
        self.current_price = price;
        if price > self.highest_price {
            self.highest_price = price;
            true
        } else {
            false
        }
    }

    /// 璁＄畻鐩堜簭鐧惧垎姣?    pub fn pnl_percent(&self) -> f64 {
        if self.entry_price_sol > 0.0 {
            ((self.current_price - self.entry_price_sol) / self.entry_price_sol) * 100.0
        } else {
            0.0
        }
    }

    /// 璁＄畻浠庢渶楂樼偣鍥炴挙鐧惧垎姣?    pub fn drawdown_percent(&self) -> f64 {
        if self.highest_price > 0.0 {
            ((self.highest_price - self.current_price) / self.highest_price) * 100.0
        } else {
            0.0
        }
    }

    /// 鎸佷粨鏃堕棿锛堢锛?    pub fn held_seconds(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }

    /// 姝㈡崯锛氫粎 Active 鐘舵€侊紙CONFIRMING 闃舵浠锋牸涓嶅噯纭紝涔板叆鏈‘璁ゆ椂涓嶈Е鍙戞鎹燂級
    pub fn can_check_stop_loss(&self) -> bool {
        self.state == PositionState::Active
    }

    /// 姝㈢泩 + 绉诲姩姝㈡崯锛氫粎 Active锛堢‘璁ゅ悗鎵嶇敓鏁堬紝闃叉姤浠疯櫄楂樿Е鍙戝亣姝㈢泩锛?    pub fn can_check_take_profit(&self) -> bool {
        self.state == PositionState::Active
    }

    /// 鏄惁鍙互鍗栧嚭
    pub fn can_sell(&self) -> bool {
        matches!(
            self.state,
            PositionState::Active | PositionState::Confirming
        )
    }

    pub fn can_auto_sell(&self, max_attempts: u32) -> bool {
        self.can_sell() && !self.max_sell_attempts_reached(max_attempts)
    }

    /// 鏈€澶ч噸璇曟鏁?    pub fn max_sell_attempts_reached(&self, max: u32) -> bool {
        self.sell_attempts >= max
    }
}

/// 鍗栧嚭鍘熷洜
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SellReason {
    TakeProfit,
    StopLoss,
    TrailingStop,
    MaxLifetime,
    Manual,
    /// 璺熻仾鏄庨挶鍗栧嚭锛堢洰鏍囬挶鍖呭崠鍑哄悓涓€浠ｅ竵鏃惰Е鍙戯級
    FollowSell,
}

impl std::fmt::Display for SellReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SellReason::TakeProfit => write!(f, "TAKE_PROFIT"),
            SellReason::StopLoss => write!(f, "STOP_LOSS"),
            SellReason::TrailingStop => write!(f, "TRAILING_STOP"),
            SellReason::MaxLifetime => write!(f, "MAX_LIFETIME"),
            SellReason::Manual => write!(f, "MANUAL"),
            SellReason::FollowSell => write!(f, "FOLLOW_SELL"),
        }
    }
}

/// 鍗栧嚭淇″彿
#[derive(Debug, Clone)]
pub struct SellSignal {
    pub position_key: PositionKey,
    pub group_name: String,
    pub reason: SellReason,
    pub current_price: f64,
    pub pnl_percent: f64,
}
