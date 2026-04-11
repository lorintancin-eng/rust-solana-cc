use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::debug;

use crate::autosell::{AutoSellManager, Position, SellReason, SellSignal};
use crate::config::{AppConfig, SELL_MODE_FOLLOW, SELL_MODE_TP_SL};
use crate::consensus::ConsensusEngine;
use crate::groups::{CopyGroup, GroupManager};
use crate::tx::sell_executor::SellExecutor;
use crate::utils::sol_price::SolUsdPrice;

pub enum TgEvent {
    ConsensusReached {
        group_name: String,
        mint: Pubkey,
        wallets: Vec<Pubkey>,
    },
    BuySubmitted {
        group_name: String,
        mint: Pubkey,
        sol_amount: f64,
        latency_ms: u64,
    },
    BuyConfirmed {
        group_name: String,
        mint: Pubkey,
        token_name: String,
        spent_sol: f64,
        cost_price_usd: String,
        mcap_usd: String,
    },
    BuyFailed {
        group_name: String,
        mint: Pubkey,
        reason: String,
    },
    SellSuccess {
        group_name: String,
        mint: Pubkey,
        token_name: String,
        reason: String,
        pnl_percent: f64,
        tx_sig: String,
    },
    SellFailed {
        group_name: String,
        mint: Pubkey,
        reason: String,
    },
}

#[derive(Clone)]
pub struct TgNotifier {
    tx: mpsc::UnboundedSender<TgEvent>,
    enabled: bool,
}

impl TgNotifier {
    pub fn send(&self, event: TgEvent) {
        if self.enabled {
            let _ = self.tx.send(event);
        }
    }

    pub fn noop() -> Self {
        let (tx, _) = mpsc::unbounded_channel();
        Self { tx, enabled: false }
    }

    pub fn from_sender(tx: mpsc::UnboundedSender<TgEvent>) -> Self {
        Self { tx, enabled: true }
    }
}

pub struct TgStats {
    pub started_at: Instant,
    pub grpc_events: AtomicU64,
    pub buy_attempts: AtomicU64,
    pub buy_success: AtomicU64,
    pub buy_failed: AtomicU64,
}

impl TgStats {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            grpc_events: AtomicU64::new(0),
            buy_attempts: AtomicU64::new(0),
            buy_success: AtomicU64::new(0),
            buy_failed: AtomicU64::new(0),
        }
    }
}

pub struct TgBot {
    token: String,
    chat_id: String,
    http: reqwest::Client,
    auto_sell: Arc<AutoSellManager>,
    consensus: Arc<ConsensusEngine>,
    config: AppConfig,
    groups: Arc<GroupManager>,
    sell_signal_tx: mpsc::UnboundedSender<SellSignal>,
    _sell_executor: Arc<SellExecutor>,
    is_running: Arc<AtomicBool>,
    stats: Arc<TgStats>,
    _sol_usd: SolUsdPrice,
    event_rx: Option<mpsc::UnboundedReceiver<TgEvent>>,
}

impl TgBot {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        config: AppConfig,
        groups: Arc<GroupManager>,
        auto_sell: Arc<AutoSellManager>,
        consensus: Arc<ConsensusEngine>,
        sell_signal_tx: mpsc::UnboundedSender<SellSignal>,
        sell_executor: Arc<SellExecutor>,
        is_running: Arc<AtomicBool>,
        stats: Arc<TgStats>,
        sol_usd: SolUsdPrice,
        event_rx: mpsc::UnboundedReceiver<TgEvent>,
    ) -> Self {
        Self {
            token: config.telegram_bot_token.clone().unwrap_or_default(),
            chat_id: config.telegram_chat_id.clone().unwrap_or_default(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("telegram client"),
            auto_sell,
            consensus,
            config,
            groups,
            sell_signal_tx,
            _sell_executor: sell_executor,
            is_running,
            stats,
            _sol_usd: sol_usd,
            event_rx: Some(event_rx),
        }
    }

    pub async fn run(mut self) {
        let mut offset: i64 = 0;
        let mut event_rx = self.event_rx.take().expect("event_rx already taken");

        self.send_msg(
            "<b>Copy trader online</b>\n\nUse /start to begin listening.\nUse /help to view group commands.",
        )
        .await;

        loop {
            tokio::select! {
                Some(event) = event_rx.recv() => self.handle_event(event).await,
                result = self.get_updates(offset) => {
                    match result {
                        Ok(updates) => {
                            for update in updates {
                                let uid = update["update_id"].as_i64().unwrap_or(0);
                                if uid >= offset {
                                    offset = uid + 1;
                                }
                                self.handle_update(&update).await;
                            }
                        }
                        Err(err) => {
                            debug!("telegram getUpdates error: {}", err);
                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        }
                    }
                }
            }
        }
    }

    async fn get_updates(&self, offset: i64) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!("https://api.telegram.org/bot{}/getUpdates", self.token);
        let resp: serde_json::Value = self
            .http
            .get(&url)
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", "30".to_string()),
                ("allowed_updates", r#"["message"]"#.to_string()),
            ])
            .send()
            .await?
            .json()
            .await?;

        if resp["ok"].as_bool() != Some(true) {
            anyhow::bail!("telegram getUpdates failed: {}", resp);
        }

        Ok(resp["result"].as_array().cloned().unwrap_or_default())
    }

    async fn send_msg(&self, text: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        let _ = self.http.post(&url).json(&body).send().await;
    }

    async fn handle_update(&self, update: &serde_json::Value) {
        let Some(msg) = update.get("message") else {
            return;
        };
        let chat_id = msg["chat"]["id"].as_i64().unwrap_or(0).to_string();
        if chat_id != self.chat_id {
            return;
        }

        let Some(text) = msg["text"].as_str() else {
            return;
        };

        let parts: Vec<&str> = text.split_whitespace().collect();
        let cmd = parts
            .first()
            .copied()
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("");

        match cmd {
            "/help" => self.cmd_help().await,
            "/start" => self.cmd_start().await,
            "/stop" => self.cmd_stop().await,
            "/status" => self.cmd_status().await,
            "/groups" => self.cmd_groups().await,
            "/groupadd" => self.cmd_groupadd(&parts[1..]).await,
            "/groupdel" => self.cmd_groupdel(&parts[1..]).await,
            "/usegroup" => self.cmd_usegroup(&parts[1..]).await,
            "/groupon" => self.cmd_group_enabled(&parts[1..], true).await,
            "/groupoff" => self.cmd_group_enabled(&parts[1..], false).await,
            "/set" => self.cmd_set(&parts[1..]).await,
            "/wallets" => self.cmd_wallets().await,
            "/addwallet" => self.cmd_addwallet(&parts[1..]).await,
            "/rmwallet" => self.cmd_rmwallet(&parts[1..]).await,
            "/sellmode" => self.cmd_sellmode(&parts[1..]).await,
            "/pos" => self.cmd_positions(&parts[1..]).await,
            "/sellall" => self.cmd_sellall(&parts[1..]).await,
            "/stats" => self.cmd_stats().await,
            _ => {}
        }
    }

    async fn cmd_help(&self) {
        self.send_msg(
            "<b>Commands</b>\n\n\
/start\n\
/stop\n\
/status\n\
/groups\n\
/groupadd &lt;name&gt;\n\
/groupdel &lt;group_id&gt;\n\
/usegroup &lt;group_id&gt;\n\
/groupon &lt;group_id&gt;\n\
/groupoff &lt;group_id&gt;\n\
/set &lt;key&gt; &lt;value&gt;\n\
/sellmode [follow|tp_sl]\n\
/wallets\n\
/addwallet &lt;pubkey&gt;\n\
/rmwallet &lt;pubkey&gt;\n\
/pos [group_id]\n\
/sellall [group_id]\n\
/stats\n\n\
Available keys: buy, min_buy, tp, sl, trailing, slippage, sell_slippage, consensus, hold, tip_buy, tip_sell, mode, enabled",
        )
        .await;
    }

    async fn cmd_start(&self) {
        self.is_running.store(true, Ordering::Relaxed);
        self.send_msg("Listening started.").await;
    }

    async fn cmd_stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
        self.send_msg("Listening stopped.").await;
    }

    async fn cmd_status(&self) {
        let mut text = format!(
            "<b>Runtime Status</b>\n\nState: {}\nGroups: {}\nOpen positions: {}\nPending consensus: {}",
            if self.is_running.load(Ordering::Relaxed) {
                "RUNNING"
            } else {
                "STOPPED"
            },
            self.groups.all_groups().len(),
            self.auto_sell.get_active_positions().len(),
            self.consensus.pending_count(),
        );

        if let Some(group) = self.groups.selected_group() {
            text.push_str("\n\n");
            text.push_str(&format_group(&group));
        }

        self.send_msg(&text).await;
    }

    async fn cmd_groups(&self) {
        let selected = self.groups.selected_group_id();
        let mut text = "<b>Copy Groups</b>".to_string();

        for group in self.groups.all_groups() {
            let marker = if selected.as_deref() == Some(group.id.as_str()) {
                "*"
            } else {
                "-"
            };
            text.push_str(&format!(
                "\n\n{} <b>{}</b> ({})\nStatus: {}\nWallets: {}\nMode: {}",
                marker,
                group.name,
                group.id,
                if group.enabled { "ON" } else { "OFF" },
                group.wallets.len(),
                sell_mode_label(group.sell_mode),
            ));
        }

        self.send_msg(&text).await;
    }

    async fn cmd_groupadd(&self, args: &[&str]) {
        if args.is_empty() {
            self.send_msg("Usage: /groupadd <name>").await;
            return;
        }

        let group = self.groups.add_group(args.join(" "), &self.config);
        self.send_msg(&format!(
            "Group created: <b>{}</b> ({})",
            group.name, group.id
        ))
        .await;
    }

    async fn cmd_groupdel(&self, args: &[&str]) {
        let Some(group_id) = args.first() else {
            self.send_msg("Usage: /groupdel <group_id>").await;
            return;
        };

        match self.groups.delete_group(group_id) {
            Ok(()) => self.send_msg("Group deleted.").await,
            Err(err) => self.send_msg(&err).await,
        }
    }

    async fn cmd_usegroup(&self, args: &[&str]) {
        let Some(group_id) = args.first() else {
            self.send_msg("Usage: /usegroup <group_id>").await;
            return;
        };

        match self.groups.set_selected_group(group_id) {
            Ok(()) => self.send_msg("Selected group changed.").await,
            Err(err) => self.send_msg(&err).await,
        }
    }

    async fn cmd_group_enabled(&self, args: &[&str], enabled: bool) {
        let Some(group_id) = args.first() else {
            self.send_msg(if enabled {
                "Usage: /groupon <group_id>"
            } else {
                "Usage: /groupoff <group_id>"
            })
            .await;
            return;
        };

        match self.groups.set_group_enabled(group_id, enabled) {
            Ok(()) => {
                self.send_msg(if enabled {
                    "Group enabled."
                } else {
                    "Group disabled."
                })
                .await;
            }
            Err(err) => self.send_msg(&err).await,
        }
    }

    async fn cmd_set(&self, args: &[&str]) {
        let Some(mut group) = self.groups.selected_group() else {
            self.send_msg("No selected group.").await;
            return;
        };

        if args.is_empty() {
            self.send_msg(&format_group(&group)).await;
            return;
        }

        if args.len() < 2 {
            self.send_msg("Usage: /set <key> <value>").await;
            return;
        }

        let key = args[0].to_ascii_lowercase();
        let value = if args.len() >= 3 && args[1] == "=" {
            args[2]
        } else {
            args[1]
        };
        let value = value.trim_end_matches('%');

        let result: Result<String, String> = match key.as_str() {
            "buy" => value
                .parse::<f64>()
                .map(|v| {
                    group.buy_sol_amount = v;
                    format!("buy = {} SOL", v)
                })
                .map_err(|err| err.to_string()),
            "min_buy" => value
                .parse::<f64>()
                .map(|v| {
                    group.min_target_buy_sol = v;
                    format!("min_buy = {} SOL", v)
                })
                .map_err(|err| err.to_string()),
            "tp" => value
                .parse::<f64>()
                .map(|v| {
                    group.take_profit_percent = v;
                    format!("tp = {}%", v)
                })
                .map_err(|err| err.to_string()),
            "sl" => value
                .parse::<f64>()
                .map(|v| {
                    group.stop_loss_percent = v.abs();
                    format!("sl = {}%", v.abs())
                })
                .map_err(|err| err.to_string()),
            "trailing" => value
                .parse::<f64>()
                .map(|v| {
                    group.trailing_stop_percent = v;
                    format!("trailing = {}%", v)
                })
                .map_err(|err| err.to_string()),
            "slippage" => value
                .parse::<u64>()
                .map(|v| {
                    group.slippage_bps = v;
                    format!("slippage = {} bps", v)
                })
                .map_err(|err| err.to_string()),
            "sell_slippage" => value
                .parse::<u64>()
                .map(|v| {
                    group.sell_slippage_bps = v;
                    format!("sell_slippage = {} bps", v)
                })
                .map_err(|err| err.to_string()),
            "consensus" => value
                .parse::<usize>()
                .map(|v| {
                    group.consensus_min_wallets = v.max(1);
                    format!("consensus = {}", group.consensus_min_wallets)
                })
                .map_err(|err| err.to_string()),
            "hold" => value
                .parse::<u64>()
                .map(|v| {
                    group.max_hold_seconds = v * 60;
                    format!("hold = {} minutes", v)
                })
                .map_err(|err| err.to_string()),
            "tip_buy" => value
                .parse::<u64>()
                .map(|v| {
                    group.tip_buy_lamports = v;
                    format!("tip_buy = {} lamports", v)
                })
                .map_err(|err| err.to_string()),
            "tip_sell" => value
                .parse::<u64>()
                .map(|v| {
                    group.tip_sell_lamports = v;
                    format!("tip_sell = {} lamports", v)
                })
                .map_err(|err| err.to_string()),
            "mode" | "sell_mode" => parse_sell_mode(value).map(|mode| {
                group.sell_mode = mode;
                format!("mode = {}", sell_mode_label(mode))
            }),
            "enabled" => parse_bool_flag(value).map(|enabled| {
                group.enabled = enabled;
                format!("enabled = {}", enabled)
            }),
            _ => Err(format!("Unknown key: {}", key)),
        };

        match result {
            Ok(message) => {
                self.groups.replace_group(group);
                self.send_msg(&message).await;
            }
            Err(err) => self.send_msg(&err).await,
        }
    }

    async fn cmd_wallets(&self) {
        let Some(group) = self.groups.selected_group() else {
            self.send_msg("No selected group.").await;
            return;
        };

        let mut text = format!("<b>{}</b> ({})", group.name, group.id);
        if group.wallets.is_empty() {
            text.push_str("\nNo wallets.");
        } else {
            for (index, wallet) in group.wallets.iter().enumerate() {
                text.push_str(&format!(
                    "\n{}. <code>{}</code>",
                    index + 1,
                    wallet
                ));
            }
        }

        self.send_msg(&text).await;
    }

    async fn cmd_addwallet(&self, args: &[&str]) {
        let Some(group) = self.groups.selected_group() else {
            self.send_msg("No selected group.").await;
            return;
        };

        let Some(raw_wallet) = args.first() else {
            self.send_msg("Usage: /addwallet <pubkey>").await;
            return;
        };

        match Pubkey::from_str(raw_wallet) {
            Ok(wallet) => match self.groups.add_wallet(&group.id, wallet) {
                Ok(()) => self.send_msg("Wallet added. Restart stream to apply.").await,
                Err(err) => self.send_msg(&err).await,
            },
            Err(_) => self.send_msg("Invalid wallet address.").await,
        }
    }

    async fn cmd_rmwallet(&self, args: &[&str]) {
        let Some(group) = self.groups.selected_group() else {
            self.send_msg("No selected group.").await;
            return;
        };

        let Some(raw_wallet) = args.first() else {
            self.send_msg("Usage: /rmwallet <pubkey>").await;
            return;
        };

        match Pubkey::from_str(raw_wallet) {
            Ok(wallet) => match self.groups.remove_wallet(&group.id, &wallet) {
                Ok(()) => self.send_msg("Wallet removed. Restart stream to apply.").await,
                Err(err) => self.send_msg(&err).await,
            },
            Err(_) => self.send_msg("Invalid wallet address.").await,
        }
    }

    async fn cmd_sellmode(&self, args: &[&str]) {
        let Some(mut group) = self.groups.selected_group() else {
            self.send_msg("No selected group.").await;
            return;
        };

        if args.is_empty() {
            self.send_msg(&format!(
                "Current sell mode for <b>{}</b>: <b>{}</b>",
                group.name,
                sell_mode_label(group.sell_mode),
            ))
            .await;
            return;
        }

        match parse_sell_mode(args[0]) {
            Ok(mode) => {
                group.sell_mode = mode;
                self.groups.replace_group(group.clone());
                self.send_msg(&format!(
                    "Sell mode updated for <b>{}</b>: <b>{}</b>",
                    group.name,
                    sell_mode_label(mode),
                ))
                .await;
            }
            Err(err) => self.send_msg(&err).await,
        }
    }

    async fn cmd_positions(&self, args: &[&str]) {
        let positions = if let Some(group_id) = args.first() {
            self.auto_sell.get_group_positions(group_id)
        } else {
            self.auto_sell.get_active_positions()
        };

        if positions.is_empty() {
            self.send_msg("No open positions.").await;
            return;
        }

        self.send_msg(&format_positions(&positions)).await;
    }

    async fn cmd_sellall(&self, args: &[&str]) {
        let positions = if let Some(group_id) = args.first() {
            self.auto_sell.get_group_positions(group_id)
        } else {
            self.auto_sell.get_active_positions()
        };

        if positions.is_empty() {
            self.send_msg("No positions to sell.").await;
            return;
        }

        for pos in positions {
            let _ = self.sell_signal_tx.send(SellSignal {
                position_key: pos.key(),
                group_name: pos.group.name.clone(),
                reason: SellReason::Manual,
                current_price: pos.current_price,
                pnl_percent: pos.pnl_percent(),
            });
        }

        self.send_msg("Manual sell signals queued.").await;
    }

    async fn cmd_stats(&self) {
        let text = format!(
            "<b>Runtime Stats</b>\n\nUptime: {}\ngRPC events: {}\nBuy attempts: {}\nBuy success: {}\nBuy failed: {}\nPositions tracked: {}",
            fmt_time(self.stats.started_at.elapsed().as_secs()),
            self.stats.grpc_events.load(Ordering::Relaxed),
            self.stats.buy_attempts.load(Ordering::Relaxed),
            self.stats.buy_success.load(Ordering::Relaxed),
            self.stats.buy_failed.load(Ordering::Relaxed),
            self.auto_sell.position_count(),
        );
        self.send_msg(&text).await;
    }

    async fn handle_event(&self, event: TgEvent) {
        let text = match event {
            TgEvent::ConsensusReached {
                group_name,
                mint,
                wallets,
            } => {
                let wallets = wallets
                    .iter()
                    .map(short_pubkey)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "<b>Consensus reached</b>\n\nGroup: <b>{}</b>\nMint: <code>{}</code>\nWallets: {}",
                    group_name, mint, wallets
                )
            }
            TgEvent::BuySubmitted {
                group_name,
                mint,
                sol_amount,
                latency_ms,
            } => {
                format!(
                    "<b>Buy submitted</b>\n\nGroup: <b>{}</b>\nMint: <code>{}</code>\nAmount: {:.4} SOL\nLatency: {}ms",
                    group_name,
                    short_pubkey(&mint),
                    sol_amount,
                    latency_ms,
                )
            }
            TgEvent::BuyConfirmed {
                group_name,
                token_name,
                spent_sol,
                cost_price_usd,
                mcap_usd,
                ..
            } => {
                self.stats.buy_success.fetch_add(1, Ordering::Relaxed);
                format!(
                    "<b>Buy confirmed</b>\n\nGroup: <b>{}</b>\nToken: {}\nSpent: {:.4} SOL\nCost: {}\nMcap: {}",
                    group_name, token_name, spent_sol, cost_price_usd, mcap_usd
                )
            }
            TgEvent::BuyFailed {
                group_name,
                mint,
                reason,
            } => {
                self.stats.buy_failed.fetch_add(1, Ordering::Relaxed);
                format!(
                    "<b>Buy failed</b>\n\nGroup: <b>{}</b>\nMint: <code>{}</code>\nReason: {}",
                    group_name,
                    short_pubkey(&mint),
                    reason,
                )
            }
            TgEvent::SellSuccess {
                group_name,
                token_name,
                reason,
                pnl_percent,
                tx_sig,
                ..
            } => {
                format!(
                    "<b>Sell success</b>\n\nGroup: <b>{}</b>\nToken: {}\nReason: {}\nPnL: {:+.2}%\n<a href=\"https://solscan.io/tx/{}\">Solscan</a>",
                    group_name, token_name, reason, pnl_percent, tx_sig
                )
            }
            TgEvent::SellFailed {
                group_name,
                mint,
                reason,
            } => {
                format!(
                    "<b>Sell failed</b>\n\nGroup: <b>{}</b>\nMint: <code>{}</code>\nReason: {}",
                    group_name,
                    short_pubkey(&mint),
                    reason,
                )
            }
        };

        self.send_msg(&text).await;
    }
}

fn parse_sell_mode(value: &str) -> Result<u8, String> {
    match value.to_ascii_lowercase().as_str() {
        "follow" | "follow_sell" | "follow-sell" => Ok(SELL_MODE_FOLLOW),
        "tp" | "sl" | "tp_sl" | "tpsl" | "tp-sl" => Ok(SELL_MODE_TP_SL),
        _ => Err("Invalid sell mode. Use follow or tp_sl.".to_string()),
    }
}

fn parse_bool_flag(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Ok(true),
        "0" | "off" | "false" | "no" => Ok(false),
        _ => Err("Invalid boolean value. Use on/off.".to_string()),
    }
}

fn sell_mode_label(mode: u8) -> &'static str {
    if mode == SELL_MODE_FOLLOW {
        "FOLLOW"
    } else {
        "TP/SL"
    }
}

fn format_group(group: &CopyGroup) -> String {
    format!(
        "<b>Selected Group</b>\n\nName: <b>{}</b>\nID: {}\nEnabled: {}\nMode: {}\nWallets: {}\nBuy: {} SOL\nMin buy: {} SOL\nTP: {}%\nSL: {}%\nTrailing: {}%\nSlippage: {} bps\nSell slippage: {} bps\nConsensus: {}\nHold: {} min\nTip buy: {} lamports\nTip sell: {} lamports",
        group.name,
        group.id,
        group.enabled,
        sell_mode_label(group.sell_mode),
        group.wallets.len(),
        group.buy_sol_amount,
        group.min_target_buy_sol,
        group.take_profit_percent,
        group.stop_loss_percent,
        group.trailing_stop_percent,
        group.slippage_bps,
        group.sell_slippage_bps,
        group.consensus_min_wallets,
        group.max_hold_seconds / 60,
        group.tip_buy_lamports,
        group.tip_sell_lamports,
    )
}

fn format_positions(positions: &[Position]) -> String {
    let mut text = format!("<b>Positions</b> ({})", positions.len());

    for (index, pos) in positions.iter().enumerate() {
        let mint = pos.token_mint.to_string();
        let token_name = if pos.token_name.is_empty() {
            format!("{}..{}", &mint[..6], &mint[mint.len() - 4..])
        } else {
            pos.token_name.clone()
        };

        text.push_str(&format!(
            "\n\n{}. <b>{}</b> [{}]\nState: {}\nPnL: {:+.2}%\nHeld: {}",
            index + 1,
            token_name,
            pos.group.name,
            pos.state,
            pos.pnl_percent(),
            fmt_time(pos.held_seconds()),
        ));
    }

    text
}

fn short_pubkey(pubkey: &Pubkey) -> String {
    let value = pubkey.to_string();
    format!("{}..{}", &value[..6], &value[value.len() - 4..])
}

fn fmt_time(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

pub async fn send_shutdown_notification(bot_token: &str, chat_id: &str) {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": "<b>Copy trader offline</b>",
        "parse_mode": "HTML",
    });

    if let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        let _ = client.post(&url).json(&body).send().await;
    }
}
