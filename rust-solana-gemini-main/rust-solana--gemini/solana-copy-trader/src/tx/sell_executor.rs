use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::autosell::{AutoSellManager, SellAccountSnapshot, SellSignal};
use crate::config::{AppConfig, DynConfig};
use crate::grpc::{AccountSubscriber, AtaBalanceCache, BondingCurveCache};
use crate::processor::prefetch::PrefetchCache;
use crate::processor::pumpfun::PumpfunProcessor;
use crate::telegram::{TgEvent, TgNotifier};
use crate::tx::blockhash::BlockhashCache;
use crate::tx::builder::TxBuilder;
use crate::tx::jupiter::JupiterSeller;
use crate::tx::sender::TxSender;

const MAX_SELL_RETRIES: u32 = 3;

#[derive(Debug, Clone, Copy)]
struct SellPathTimings {
    signal_queue: Duration,
    snapshot_load: Duration,
    bc_lookup: Duration,
    quote_build: Duration,
    build: Duration,
    send_call: Duration,
    total: Duration,
}

fn format_latency(duration: Duration) -> String {
    if duration.as_millis() > 0 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}us", duration.as_micros())
    }
}

pub struct SellExecutor {
    config: AppConfig,
    dyn_config: Arc<DynConfig>,
    rpc_client: Arc<RpcClient>,
    pumpfun: Arc<PumpfunProcessor>,
    tx_sender: Arc<TxSender>,
    blockhash_cache: BlockhashCache,
    auto_sell: Arc<AutoSellManager>,
    bc_cache: BondingCurveCache,
    ata_cache: AtaBalanceCache,
    prefetch_cache: Arc<PrefetchCache>,
    account_subscriber: Arc<AccountSubscriber>,
    jupiter: JupiterSeller,
    tg: TgNotifier,
}

impl SellExecutor {
    pub fn new(
        config: AppConfig,
        dyn_config: Arc<DynConfig>,
        rpc_client: Arc<RpcClient>,
        pumpfun: Arc<PumpfunProcessor>,
        tx_sender: Arc<TxSender>,
        blockhash_cache: BlockhashCache,
        auto_sell: Arc<AutoSellManager>,
        bc_cache: BondingCurveCache,
        ata_cache: AtaBalanceCache,
        prefetch_cache: Arc<PrefetchCache>,
        account_subscriber: Arc<AccountSubscriber>,
        tg: TgNotifier,
    ) -> Self {
        Self {
            config,
            dyn_config,
            rpc_client,
            pumpfun,
            tx_sender,
            blockhash_cache,
            auto_sell,
            bc_cache,
            ata_cache,
            prefetch_cache,
            account_subscriber,
            jupiter: JupiterSeller::new(),
            tg,
        }
    }

    /// Pump.fun direct sell: local instruction build -> TxBuilder -> multi-channel send.
    /// Sell path never uses 0slot to avoid extra fee broadcasts on exit.
    async fn try_pumpfun_sell(
        &self,
        mint: &Pubkey,
        token_amount: u64,
        signal_received_at: std::time::Instant,
    ) -> Result<(String, SellPathTimings)> {
        let sell_start = std::time::Instant::now();
        let mut timings = SellPathTimings {
            signal_queue: sell_start.saturating_duration_since(signal_received_at),
            snapshot_load: Duration::default(),
            bc_lookup: Duration::default(),
            quote_build: Duration::default(),
            build: Duration::default(),
            send_call: Duration::default(),
            total: Duration::default(),
        };

        let snapshot_load_start = std::time::Instant::now();
        let snapshot = self
            .auto_sell
            .get_position(mint)
            .and_then(|pos| pos.sell_snapshot)
            .or_else(|| {
                self.prefetch_cache.get(mint).map(|pf| SellAccountSnapshot {
                    bonding_curve: pf.bonding_curve,
                    associated_bonding_curve: pf.associated_bonding_curve,
                    user_ata: pf.user_ata,
                    token_program: pf.token_program,
                    mirror_accounts: pf.mirror_accounts,
                    source_wallet: pf.source_wallet,
                })
            })
            .ok_or_else(|| anyhow::anyhow!("no sell snapshot"))?;
        timings.snapshot_load = snapshot_load_start.elapsed();

        if snapshot.mirror_accounts.is_empty() {
            anyhow::bail!("no mirror_accounts");
        }

        let bc_lookup_start = std::time::Instant::now();
        let bc_state = if let Some(state) = self.bc_cache.get(mint) {
            state
        } else {
            self.pumpfun
                .prefetch_bonding_curve(&snapshot.bonding_curve)
                .await?
        };
        timings.bc_lookup = bc_lookup_start.elapsed();

        if bc_state.complete {
            anyhow::bail!("bonding curve completed");
        }

        let quote_build_start = std::time::Instant::now();
        let expected_sol = bc_state.token_to_sol_quote(token_amount);
        let slippage = self.dyn_config.sell_slippage_bps();
        let min_sol_output = expected_sol.saturating_sub(expected_sol * slippage / 10_000);

        info!(
            "Pump.fun direct sell: {} tokens -> ~{:.4} SOL (min: {:.4}, slippage: {}bps)",
            token_amount,
            expected_sol as f64 / 1e9,
            min_sol_output as f64 / 1e9,
            slippage,
        );
        timings.quote_build = quote_build_start.elapsed();

        let creator = bc_state
            .creator
            .ok_or_else(|| anyhow::anyhow!("BC state missing creator"))?;

        let sell_ix = self.pumpfun.build_sell_instruction_from_mirror(
            &self.config.pubkey,
            &snapshot.user_ata,
            &snapshot.mirror_accounts,
            token_amount,
            min_sol_output,
            &snapshot.token_program,
            &creator,
            bc_state.is_cashback,
        );

        let mirror = crate::processor::MirrorInstruction {
            swap_instructions: vec![sell_ix],
            pre_instructions: vec![],
            post_instructions: vec![],
            token_mint: *mint,
            sol_amount: expected_sol,
        };

        let (blockhash, _) = self.blockhash_cache.get_sync();
        let tx_build_start = std::time::Instant::now();
        let transaction = if self.config.jito_enabled {
            let tip = self.tx_sender.random_jito_tip_account();
            TxBuilder::build_jito_bundle_transaction(
                &mirror,
                &self.config,
                &self.config.keypair,
                blockhash,
                &tip,
                self.dyn_config.jito_sell_tip_lamports(),
                &[],
            )?
        } else {
            TxBuilder::build_transaction(
                &mirror,
                &self.config,
                &self.config.keypair,
                blockhash,
                &[],
            )?
        };
        timings.build = tx_build_start.elapsed();
        let send_call_start = std::time::Instant::now();
        let sig = self.tx_sender.fire_and_forget_without_0slot(&transaction)?;
        timings.send_call = send_call_start.elapsed();
        timings.total = sell_start.elapsed();
        Ok((
            sig.to_string(),
            timings,
        ))
    }

    /// Handle sell signal, prefer Pump.fun direct sell, then fallback to Jupiter.
    pub async fn handle_sell_signal(&self, signal: SellSignal) {
        let sell_start = std::time::Instant::now();
        let mint = signal.token_mint;

        info!(
            "收到卖出信号: {}.. | 原因: {} | 盈亏: {:.2}%",
            &mint.to_string()[..12],
            signal.reason,
            signal.pnl_percent,
        );

        const MAX_TOTAL_SELL_ATTEMPTS: u32 = 3;
        if let Some(pos) = self.auto_sell.get_position(&mint) {
            if pos.sell_attempts >= MAX_TOTAL_SELL_ATTEMPTS {
                error!(
                    "卖出已尝试 {} 次全部失败，放弃: {}",
                    pos.sell_attempts,
                    &mint.to_string()[..12],
                );
                self.auto_sell
                    .confirm_failed(&mint, "max sell attempts exceeded");
                self.account_subscriber.untrack_mint(&mint);
                self.prefetch_cache.remove(&mint);
                return;
            }
        }

        if !self.auto_sell.mark_selling(&mint) {
            warn!("仓位不在可卖出状态，跳过");
            return;
        }

        let user_ata = self
            .prefetch_cache
            .get(&mint)
            .map(|pf| pf.user_ata)
            .unwrap_or_else(|| get_associated_token_address(&self.config.pubkey, &mint));

        let token_balance = self
            .ata_cache
            .get(&mint)
            .unwrap_or_else(|| self.get_token_balance_rpc(&user_ata));

        if token_balance == 0 {
            warn!("跳过卖出: 余额为 0");
            self.auto_sell.confirm_failed(&mint, "zero balance");
            return;
        }

        let mut success = false;
        let mut last_sig = String::new();

        for attempt in 1..=MAX_SELL_RETRIES {
            info!(
                "Pump.fun direct sell attempt #{}/{}: {}.. (amount: {})",
                attempt,
                MAX_SELL_RETRIES,
                &mint.to_string()[..12],
                token_balance,
            );

            match self.try_pumpfun_sell(&mint, token_balance, sell_start).await {
                Ok((sig, timings)) => {
                    info!(
                        "Pump.fun direct sell submitted: https://solscan.io/tx/{} | signal_queue={} | snapshot={} | bc_lookup={} | quote_build={} | tx_build={} | send_call={} | total={}",
                        sig,
                        format_latency(timings.signal_queue),
                        format_latency(timings.snapshot_load),
                        format_latency(timings.bc_lookup),
                        format_latency(timings.quote_build),
                        format_latency(timings.build),
                        format_latency(timings.send_call),
                        format_latency(timings.total),
                    );

                    let confirmed = self.wait_sell_confirm(&mint, &sig, 10).await;
                    if confirmed {
                        info!(
                            "Pump.fun direct sell confirmed: {} | {}ms",
                            &sig[..16],
                            sell_start.elapsed().as_millis(),
                        );
                        last_sig = sig;
                        success = true;
                        break;
                    } else {
                        warn!(
                            "Pump.fun direct sell unconfirmed or failed: {} (retry)",
                            &sig[..16]
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Pump.fun direct sell attempt #{} failed: {} -> fallback Jupiter",
                        attempt, e
                    );
                    match self.try_jupiter_sell(&mint, token_balance).await {
                        Ok(sig) => {
                            info!(
                                "Jupiter sell submitted: https://solscan.io/tx/{} | {}ms",
                                sig,
                                sell_start.elapsed().as_millis(),
                            );
                            let confirmed = self.wait_sell_confirm(&mint, &sig, 10).await;
                            if confirmed {
                                last_sig = sig;
                                success = true;
                                break;
                            }
                        }
                        Err(je) => warn!("Jupiter fallback also failed: {}", je),
                    }
                    if attempt < MAX_SELL_RETRIES {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        }

        if success {
            self.try_close_ata(&mint, &user_ata).await;
            self.auto_sell.mark_closed(&mint, last_sig.clone());
            self.ata_cache.remove(&mint);
            self.account_subscriber.untrack_mint(&mint);
            self.prefetch_cache.remove(&mint);

            let mint_str = mint.to_string();
            self.tg.send(TgEvent::SellSuccess {
                mint,
                token_name: format!("{}..{}", &mint_str[..6], &mint_str[mint_str.len() - 4..]),
                reason: signal.reason.to_string(),
                pnl_percent: signal.pnl_percent,
                tx_sig: last_sig,
            });
        } else {
            let remaining = self.get_token_balance_rpc(&user_ata);
            if remaining > 0 {
                warn!(
                    "Sell failed but {} tokens remain, revert to Active: {}",
                    remaining,
                    &mint.to_string()[..12],
                );
                self.auto_sell.revert_to_active(&mint);
            } else {
                warn!(
                    "Sell failed and remaining balance is 0, mark failed: {}",
                    &mint.to_string()[..12],
                );
                self.auto_sell
                    .confirm_failed(&mint, "sell failed, zero balance");
                self.account_subscriber.untrack_mint(&mint);
                self.prefetch_cache.remove(&mint);
            }

            self.tg.send(TgEvent::SellFailed {
                mint,
                reason: "Pump.fun direct sell + Jupiter fallback both failed".to_string(),
            });
        }
    }

    /// Build and send a Jupiter sell transaction.
    async fn try_jupiter_sell(&self, mint: &Pubkey, token_amount: u64) -> Result<String> {
        let signed_tx_bytes = self
            .jupiter
            .build_sell_transaction(
                mint,
                token_amount,
                self.dyn_config.sell_slippage_bps(),
                &self.config.keypair,
            )
            .await?;

        let tx_base64 =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &signed_tx_bytes);

        let rpc_url = self.config.rpc_url.clone();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        let send_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                tx_base64,
                {
                    "encoding": "base64",
                    "skipPreflight": true,
                    "maxRetries": 0
                }
            ]
        });

        let resp: serde_json::Value = http
            .post(&rpc_url)
            .json(&send_request)
            .send()
            .await?
            .json()
            .await?;

        if let Some(error) = resp.get("error") {
            anyhow::bail!("Jupiter sell RPC send failed: {}", error);
        }

        let sig = resp["result"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Jupiter sell returned no signature"))?;

        Ok(sig.to_string())
    }

    async fn try_close_ata(&self, mint: &Pubkey, ata: &Pubkey) {
        let balance = self
            .ata_cache
            .get(mint)
            .unwrap_or_else(|| self.get_token_balance_rpc(ata));
        if balance > 0 {
            debug!("ATA still has balance {}, skip close", balance);
            return;
        }

        let token_program = self
            .prefetch_cache
            .get(mint)
            .map(|pf| pf.token_program)
            .unwrap_or_else(spl_token::id);

        let close_ix = spl_token::instruction::close_account(
            &token_program,
            ata,
            &self.config.pubkey,
            &self.config.pubkey,
            &[],
        );

        let close_ix = match close_ix {
            Ok(ix) => ix,
            Err(e) => {
                debug!("Failed to build close ATA instruction: {}", e);
                return;
            }
        };

        let rpc_bh = self.rpc_client.clone();
        let bh_result = tokio::task::spawn_blocking(move || rpc_bh.get_latest_blockhash()).await;
        let blockhash = match bh_result {
            Ok(Ok(bh)) => bh,
            _ => {
                debug!("Failed to get blockhash, skip close ATA");
                return;
            }
        };

        let tx = match crate::tx::builder::TxBuilder::build_simple(
            &[close_ix],
            &self.config.keypair,
            blockhash,
        ) {
            Ok(tx) => tx,
            Err(e) => {
                debug!("Failed to build close ATA transaction: {}", e);
                return;
            }
        };

        let rpc = self.rpc_client.clone();
        match tokio::task::spawn_blocking(move || {
            rpc.send_transaction_with_config(
                &tx,
                solana_client::rpc_config::RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
        })
        .await
        {
            Ok(Ok(sig)) => {
                info!("ATA closed: https://solscan.io/tx/{} (~0.002 SOL)", sig,);
            }
            Ok(Err(e)) => debug!("Close ATA failed: {}", e),
            Err(e) => debug!("Close ATA task error: {}", e),
        }
    }

    /// Wait for on-chain confirmation of a sell transaction.
    async fn wait_sell_confirm(&self, mint: &Pubkey, sig_str: &str, max_secs: u64) -> bool {
        use solana_sdk::signature::Signature;
        let sig = match sig_str.parse::<Signature>() {
            Ok(s) => s,
            Err(_) => return false,
        };
        let max_wait = Duration::from_secs(max_secs);
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(80);
        let user_ata = self
            .auto_sell
            .get_position(mint)
            .and_then(|pos| pos.sell_snapshot.map(|snapshot| snapshot.user_ata))
            .unwrap_or_else(|| get_associated_token_address(&self.config.pubkey, mint));
        while start.elapsed() < max_wait {
            if let Some(balance) = self.ata_cache.get(mint) {
                if balance == 0 {
                    info!(
                        "Sell confirmed via ATA cache: {} | {}",
                        &mint.to_string()[..12],
                        format_latency(start.elapsed()),
                    );
                    return true;
                }
            }

            tokio::time::sleep(poll_interval).await;
            let rpc = self.rpc_client.clone();
            let s = sig;
            let result =
                tokio::task::spawn_blocking(move || rpc.get_signature_statuses(&[s])).await;
            match result {
                Ok(Ok(resp)) => {
                    if let Some(Some(status)) = resp.value.first() {
                        if status.err.is_some() {
                            warn!("Sell failed on-chain: {:?}", status.err);
                            return false;
                        }
                        let rpc = self.rpc_client.clone();
                        let ata = user_ata;
                        let ata_balance = tokio::task::spawn_blocking(move || {
                            rpc.get_token_account_balance(&ata)
                                .map(|b| b.amount.parse::<u64>().unwrap_or(0))
                                .unwrap_or(0)
                        })
                        .await
                        .unwrap_or(0);
                        if ata_balance == 0 {
                            info!(
                                "Sell confirmed via signature+ATA: {} | {}",
                                &mint.to_string()[..12],
                                format_latency(start.elapsed()),
                            );
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Partial sell: 25% / 50% / 75% / 100%.
    pub async fn handle_partial_sell(&self, mint: &Pubkey, percent: u32) {
        let user_ata = self
            .prefetch_cache
            .get(mint)
            .map(|pf| pf.user_ata)
            .unwrap_or_else(|| get_associated_token_address(&self.config.pubkey, mint));

        let total_balance = self
            .ata_cache
            .get(mint)
            .unwrap_or_else(|| self.get_token_balance_rpc(&user_ata));

        if total_balance == 0 {
            warn!("Partial sell skipped: zero balance");
            self.tg.send(TgEvent::SellFailed {
                mint: *mint,
                reason: "zero balance".into(),
            });
            return;
        }

        let sell_amount = if percent >= 100 {
            total_balance
        } else {
            (total_balance as u128 * percent as u128 / 100) as u64
        };

        if sell_amount == 0 {
            self.tg.send(TgEvent::SellFailed {
                mint: *mint,
                reason: "sell amount is zero".into(),
            });
            return;
        }

        info!(
            "Partial sell: {}% of {} = {} tokens",
            percent,
            &mint.to_string()[..12],
            sell_amount
        );

        let mut success = false;
        let mut last_sig = String::new();

        for attempt in 1..=MAX_SELL_RETRIES {
            match self.try_pumpfun_sell(mint, sell_amount, std::time::Instant::now()).await {
                Ok((sig, timings)) => {
                    info!(
                        "Partial sell Pump.fun submitted: https://solscan.io/tx/{} | signal_queue={} | snapshot={} | bc_lookup={} | quote_build={} | tx_build={} | send_call={} | total={}",
                        sig,
                        format_latency(timings.signal_queue),
                        format_latency(timings.snapshot_load),
                        format_latency(timings.bc_lookup),
                        format_latency(timings.quote_build),
                        format_latency(timings.build),
                        format_latency(timings.send_call),
                        format_latency(timings.total),
                    );
                    let confirmed = self.wait_sell_confirm(mint, &sig, 10).await;
                    if confirmed {
                        last_sig = sig;
                        success = true;
                        break;
                    }
                }
                Err(e) => {
                    warn!(
                        "Partial sell Pump.fun attempt #{} failed: {} -> fallback Jupiter",
                        attempt, e
                    );
                    match self.try_jupiter_sell(mint, sell_amount).await {
                        Ok(sig) => {
                            let confirmed = self.wait_sell_confirm(mint, &sig, 10).await;
                            if confirmed {
                                last_sig = sig;
                                success = true;
                                break;
                            }
                        }
                        Err(je) => warn!("Partial sell Jupiter fallback also failed: {}", je),
                    }
                    if attempt < MAX_SELL_RETRIES {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        }

        let mint_str = mint.to_string();
        let short = format!("{}..{}", &mint_str[..6], &mint_str[mint_str.len() - 4..]);

        if success {
            if percent >= 100 {
                self.try_close_ata(mint, &user_ata).await;
                self.auto_sell.mark_closed(mint, last_sig.clone());
                self.ata_cache.remove(mint);
                self.account_subscriber.untrack_mint(mint);
                self.prefetch_cache.remove(mint);
            }
            self.tg.send(TgEvent::SellSuccess {
                mint: *mint,
                token_name: short,
                reason: format!("manual {}%", percent),
                pnl_percent: self
                    .auto_sell
                    .get_position(mint)
                    .map(|p| p.pnl_percent())
                    .unwrap_or(0.0),
                tx_sig: last_sig,
            });
        } else {
            self.tg.send(TgEvent::SellFailed {
                mint: *mint,
                reason: format!("{}% sell retries exhausted", percent),
            });
        }
    }

    /// RPC fallback: only used when gRPC cache misses.
    fn get_token_balance_rpc(&self, ata: &Pubkey) -> u64 {
        self.rpc_client
            .get_token_account_balance(ata)
            .map(|b| b.amount.parse::<u64>().unwrap_or(0))
            .unwrap_or(0)
    }
}
