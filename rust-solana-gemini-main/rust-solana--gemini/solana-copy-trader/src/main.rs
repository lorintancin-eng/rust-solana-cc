mod autosell;
mod config;
mod consensus;
mod group_stats;
mod groups;
mod grpc;
mod processor;
mod telegram;
mod tx;
mod utils;

use anyhow::Result;
use dashmap::DashMap;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify, Semaphore};
use tracing::{debug, error, info, warn};

use autosell::{AutoSellManager, Position, SellAccountSnapshot, SellReason, SellSignal};
use config::AppConfig;
use consensus::engine::{BuySignal, ConsensusTrigger};
use consensus::ConsensusEngine;
use groups::{CopyGroup, GroupManager, ENTRY_MODE_SMART_BUY};
use grpc::{AccountSubscriber, AccountUpdate, AtaBalanceCache, BondingCurveCache, GrpcSubscriber};
use processor::prefetch::PrefetchCache;
use processor::pumpfun::PumpfunProcessor;
use processor::DetectedTrade;
use telegram::{TgBot, TgEvent, TgNotifier, TgStats};
use tx::{
    blockhash,
    builder::TxBuilder,
    confirm::{format_mcap_usd, format_price_gmgn, BuyConfirmer},
    sell_executor::SellExecutor,
    sender::TxSender,
};
use utils::sol_price::SolUsdPrice;

type SignatureCache = Arc<DashMap<String, SignatureSeen>>;
type GroupMintDedup = Arc<DashMap<String, Instant>>;
type BondingCurveFetches = Arc<DashMap<Pubkey, Arc<Notify>>>;

const BLOCKHASH_REFRESH_MS: u64 = 120;
const PREFETCH_WAIT_MS: u64 = 8;
const BC_CACHE_WAIT_MS: u64 = 40;
const BUY_EXACT_SOL_IN_WAIT_MS: u64 = 80;
const BUY_EXECUTOR_PARALLELISM: usize = 4;
const MAX_AUTO_SELL_SIGNAL_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone)]
struct SignatureSeen {
    pre_seen: bool,
    landed_seen: bool,
    last_seen: Instant,
}

#[derive(Debug, Clone, Copy, Default)]
struct BuyPathTimings {
    queue: Duration,
    prefetch_wait: Duration,
    bc_wait: Duration,
    bc_sync_fetch: Duration,
    quote_build: Duration,
    tx_build: Duration,
    send_call: Duration,
}

fn format_latency(duration: Duration) -> String {
    if duration.as_millis() > 0 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}us", duration.as_micros())
    }
}

fn ensure_bonding_curve_fetch(
    bc_fetches: &BondingCurveFetches,
    bc_cache: &BondingCurveCache,
    pumpfun: Arc<PumpfunProcessor>,
    mint: Pubkey,
    bonding_curve: Pubkey,
) {
    if bc_cache.get(&mint).is_some() {
        return;
    }

    let notify = Arc::new(Notify::new());
    match bc_fetches.entry(mint) {
        dashmap::mapref::entry::Entry::Occupied(_) => return,
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            entry.insert(notify.clone());
        }
    }

    let bc_fetches = bc_fetches.clone();
    let bc_cache = bc_cache.clone();
    tokio::spawn(async move {
        if let Ok(state) = pumpfun.prefetch_bonding_curve(&bonding_curve).await {
            bc_cache.update(&mint, state);
        }
        bc_fetches.remove(&mint);
        notify.notify_waiters();
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    info!("==============================================");
    info!("   Solana 跟单交易系统 v1.6.48");
    info!("   RabbitStream pre-exec + Group Copy Trading");
    info!("==============================================");

    let config = AppConfig::from_env()?;
    let group_manager = GroupManager::load_or_default(&config);
    let target_wallets = group_manager.all_target_wallets();

    info!("跟单钱包地址: {}", config.pubkey);
    info!(
        "已加载 {} 个组合 | 目标钱包数 {}",
        group_manager.all_groups().len(),
        target_wallets.len(),
    );

    let rpc_client = Arc::new(RpcClient::new_with_commitment(
        config.rpc_url.clone(),
        solana_sdk::commitment_config::CommitmentConfig::confirmed(),
    ));

    let balance = rpc_client.get_balance(&config.pubkey)?;
    info!("SOL balance: {:.4}", balance as f64 / 1e9);

    let blockhash_cache = blockhash::init_blockhash_cache(&rpc_client).await?;
    let _bh_task = blockhash_cache.start_refresh_task(
        rpc_client.clone(),
        Duration::from_millis(BLOCKHASH_REFRESH_MS),
    );

    let sol_usd = SolUsdPrice::new();
    sol_usd.init(config.default_sol_usd_price).await;
    let _sol_usd_task = sol_usd.start_refresh_task();

    let bc_cache = BondingCurveCache::new();
    let ata_cache = AtaBalanceCache::new();
    let prefetch_cache = Arc::new(PrefetchCache::new(bc_cache.clone()));
    let bc_fetches: BondingCurveFetches = Arc::new(DashMap::new());

    let tx_sender = Arc::new(TxSender::new(
        config.rpc_url.clone(),
        config.secondary_rpc_url.clone(),
        config.jito_block_engine_urls.clone(),
        config.jito_enabled,
        config.jito_auth_uuid.clone(),
        config.zero_slot_urls.clone(),
    ));
    let buy_exec_limiter = Arc::new(Semaphore::new(BUY_EXECUTOR_PARALLELISM));
    let pumpfun = Arc::new(PumpfunProcessor::new(rpc_client.clone()));
    let consensus_engine = Arc::new(ConsensusEngine::new());
    let _cleanup_task = consensus_engine.start_cleanup_task();

    let auto_sell_manager = Arc::new(AutoSellManager::new(
        config.clone(),
        bc_cache.clone(),
        rpc_client.clone(),
        sol_usd.clone(),
    ));

    let is_running = Arc::new(AtomicBool::new(false));
    let tg_stats = Arc::new(TgStats::new());

    let account_subscriber = Arc::new(AccountSubscriber::new(
        config.grpc_account_url.clone(),
        config.grpc_account_token.clone(),
        bc_cache.clone(),
        ata_cache.clone(),
    ));

    let sig_cache: SignatureCache = Arc::new(DashMap::new());
    let mint_dedup: GroupMintDedup = Arc::new(DashMap::new());

    let (trade_tx, mut trade_rx) = mpsc::unbounded_channel::<DetectedTrade>();
    let (consensus_tx, mut consensus_rx) = mpsc::unbounded_channel::<ConsensusTrigger>();
    let (sell_signal_tx, mut sell_signal_rx) = mpsc::unbounded_channel::<SellSignal>();
    let (account_update_tx, account_update_rx) = mpsc::unbounded_channel::<AccountUpdate>();

    let (tg_event_tx, tg_event_rx) = mpsc::unbounded_channel::<telegram::TgEvent>();
    let tg_notifier = if config.telegram_bot_token.is_some() && config.telegram_chat_id.is_some() {
        TgNotifier::from_sender(tg_event_tx)
    } else {
        TgNotifier::noop()
    };

    let sell_executor = Arc::new(SellExecutor::new(
        config.clone(),
        rpc_client.clone(),
        pumpfun.clone(),
        tx_sender.clone(),
        blockhash_cache.clone(),
        auto_sell_manager.clone(),
        bc_cache.clone(),
        ata_cache.clone(),
        prefetch_cache.clone(),
        account_subscriber.clone(),
        tg_notifier.clone(),
    ));

    if let Some(bot_token) = config.telegram_bot_token.clone() {
        if let Some(chat_id) = config.telegram_chat_id.clone() {
            let tg_bot = TgBot::from_parts(
                config.clone(),
                group_manager.clone(),
                auto_sell_manager.clone(),
                consensus_engine.clone(),
                sell_signal_tx.clone(),
                sell_executor.clone(),
                is_running.clone(),
                tg_stats.clone(),
                sol_usd.clone(),
                tg_event_rx,
            );
            info!("Telegram bot enabled for chat {}", chat_id);
            tokio::spawn(async move {
                let _ = bot_token;
                tg_bot.run().await;
            });
        }
    }

    if config.auto_sell_enabled {
        let _grpc_monitor =
            auto_sell_manager.start_grpc_monitor(account_update_rx, sell_signal_tx.clone());
        let _fallback_monitor = auto_sell_manager.start_fallback_monitor(sell_signal_tx.clone());
    }

    let sig_cache_clone = sig_cache.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            sig_cache_clone.retain(|_, value| value.last_seen.elapsed() < Duration::from_secs(10));
        }
    });

    let prefetch_clone = prefetch_cache.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            prefetch_clone.cleanup(300);
        }
    });

    let grpc_sub = GrpcSubscriber::new(
        config.grpc_url.clone(),
        config.grpc_token.clone(),
        target_wallets.clone(),
    );
    let trade_tx_clone = trade_tx.clone();
    tokio::spawn(async move {
        loop {
            match grpc_sub.subscribe(trade_tx_clone.clone()).await {
                Ok(()) => warn!("gRPC trade stream closed, reconnecting"),
                Err(err) => error!("gRPC trade stream error: {}", err),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let acct_sub_clone = account_subscriber.clone();
    let acct_update_tx_clone = account_update_tx.clone();
    tokio::spawn(async move {
        loop {
            match acct_sub_clone.subscribe(acct_update_tx_clone.clone()).await {
                Ok(()) => warn!("gRPC account stream closed, reconnecting"),
                Err(err) => error!("gRPC account stream error: {}", err),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let sell_exec = sell_executor.clone();
    tokio::spawn(async move {
        while let Some(signal) = sell_signal_rx.recv().await {
            sell_exec.handle_sell_signal(signal).await;
        }
    });

    let exec_config = config.clone();
    let exec_rpc = rpc_client.clone();
    let exec_pumpfun = pumpfun.clone();
    let exec_blockhash = blockhash_cache.clone();
    let exec_tx_sender = tx_sender.clone();
    let exec_sol_usd = sol_usd.clone();
    let exec_auto_sell = auto_sell_manager.clone();
    let exec_prefetch = prefetch_cache.clone();
    let exec_bc_cache = bc_cache.clone();
    let exec_ata_cache = ata_cache.clone();
    let exec_acct_sub = account_subscriber.clone();
    let exec_tg = tg_notifier.clone();
    let exec_tg_stats = tg_stats.clone();
    let exec_mint_dedup = mint_dedup.clone();
    let exec_buy_limiter = buy_exec_limiter.clone();
    let exec_group_manager = group_manager.clone();
    let exec_bc_fetches = bc_fetches.clone();
    tokio::spawn(async move {
        while let Some(trigger) = consensus_rx.recv().await {
            let dedup_key = group_mint_key(&trigger.group_id, &trigger.token_mint);
            if exec_mint_dedup.contains_key(&dedup_key) {
                continue;
            }
            exec_mint_dedup.insert(dedup_key, Instant::now());

            exec_tg.send(TgEvent::ConsensusReached {
                group_name: trigger.group_name.clone(),
                mint: trigger.token_mint,
                wallets: trigger.wallets.clone(),
            });

            let Some(group) = exec_group_manager.get_group(&trigger.group_id) else {
                warn!("Missing group for consensus trigger: {}", trigger.group_id);
                continue;
            };

            let cfg = exec_config.clone();
            let rpc = exec_rpc.clone();
            let pf = exec_pumpfun.clone();
            let bh = exec_blockhash.clone();
            let sender = exec_tx_sender.clone();
            let sol = exec_sol_usd.clone();
            let auto_sell = exec_auto_sell.clone();
            let prefetch = exec_prefetch.clone();
            let bc = exec_bc_cache.clone();
            let ata = exec_ata_cache.clone();
            let acct_sub = exec_acct_sub.clone();
            let tg = exec_tg.clone();
            let stats = exec_tg_stats.clone();
            let limiter = exec_buy_limiter.clone();
            let bc_fetches = exec_bc_fetches.clone();
            let trigger_mint = trigger.token_mint;
            let trigger_wallets = trigger.wallets.clone();
            let trigger_detected_at = trigger.triggered_at;
            let zero_slot_buy_enabled = exec_group_manager.zero_slot_buy_enabled();
            let canonical_signature = trigger.canonical_signature.clone();
            let canonical_wallet = trigger.canonical_wallet;
            let canonical_token_program = trigger.canonical_token_program;
            let canonical_instruction_accounts = trigger.canonical_instruction_accounts.clone();
            let canonical_instruction_data = trigger.canonical_instruction_data.clone();
            tokio::spawn(async move {
                let _permit = limiter.acquire_owned().await.expect("buy semaphore closed");
                prefetch.prefetch_token(
                    &trigger_mint,
                    &canonical_token_program,
                    &canonical_instruction_accounts,
                    &canonical_wallet,
                    &canonical_signature,
                    1_000,
                    true,
                    &cfg,
                );
                if let Some(prefetched) = prefetch.get(&trigger_mint) {
                    ensure_bonding_curve_fetch(
                        &bc_fetches,
                        &bc,
                        pf.clone(),
                        trigger_mint,
                        prefetched.bonding_curve,
                    );
                }
                execute_buy(
                    &group,
                    &trigger_mint,
                    &trigger_wallets,
                    trigger_detected_at,
                    zero_slot_buy_enabled,
                    &canonical_instruction_data,
                    &cfg,
                    &rpc,
                    &pf,
                    &bh,
                    &sender,
                    &sol,
                    &auto_sell,
                    &prefetch,
                    &bc,
                    &bc_fetches,
                    &ata,
                    &acct_sub,
                    &tg,
                    &stats,
                )
                .await;
            });
        }
    });

    if is_running.load(Ordering::Relaxed) {
        info!("Main loop active");
    } else {
        info!("Main loop idle, waiting for TG /start");
    }

    let shutdown_token = config.telegram_bot_token.clone();
    let shutdown_chat = config.telegram_chat_id.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to create SIGTERM handler");
        #[cfg(unix)]
        tokio::select! {
            _ = ctrl_c => {},
            _ = sigterm.recv() => {},
        };
        #[cfg(not(unix))]
        ctrl_c.await.ok();

        if let (Some(token), Some(chat)) = (&shutdown_token, &shutdown_chat) {
            telegram::send_shutdown_notification(token, chat).await;
        }
        std::process::exit(0);
    });

    while let Some(trade) = trade_rx.recv().await {
        if !is_running.load(Ordering::Relaxed) {
            continue;
        }

        tg_stats.grpc_events.fetch_add(1, Ordering::Relaxed);

        if should_skip_signature(&sig_cache, &trade) {
            continue;
        }

        let matching_groups = group_manager.groups_for_wallet(&trade.source_wallet);
        if matching_groups.is_empty() {
            continue;
        }

        let token_info = extract_token_info(&trade);
        if token_info.is_none() {
            continue;
        }
        let (token_mint, token_program) = token_info.unwrap();

        if group_manager.is_blocked(&token_mint) {
            info!("Blocked mint skipped: {}", &token_mint.to_string()[..12]);
            continue;
        }

        let mut entry_groups = Vec::new();
        for group in matching_groups {
            let wants_entry = if trade.is_buy {
                group.buy_on_smart_buy()
            } else {
                group.buy_on_smart_sell()
            };

            if wants_entry {
                entry_groups.push(group.clone());
            }

            if !trade.execution_failed && !trade.is_buy && group.follow_sell_mode() {
                if let Some(position) =
                    auto_sell_manager.get_position_by_group_mint(&group.id, &token_mint)
                {
                    if position.can_sell()
                        && !position.max_sell_attempts_reached(MAX_AUTO_SELL_SIGNAL_ATTEMPTS)
                    {
                        let _ = sell_signal_tx.send(SellSignal {
                            position_key: position.key(),
                            group_name: group.name.clone(),
                            reason: SellReason::FollowSell,
                            current_price: position.current_price,
                            pnl_percent: position.pnl_percent(),
                        });
                    }
                }
            }
        }

        if entry_groups.is_empty() {
            continue;
        }

        let prefetched = prefetch_cache.prefetch_token(
            &token_mint,
            &token_program,
            &trade.instruction_accounts,
            &trade.source_wallet,
            &trade.signature,
            trade_signal_quality(&trade),
            false,
            &config,
        );
        account_subscriber.track_bonding_curve(token_mint, prefetched.bonding_curve);
        account_subscriber.track_ata(token_mint, prefetched.user_ata);

        if bc_cache.get(&token_mint).is_none() {
            ensure_bonding_curve_fetch(
                &bc_fetches,
                &bc_cache,
                pumpfun.clone(),
                token_mint,
                prefetched.bonding_curve,
            );
        }

        for group in entry_groups {
            let target_instruction_data = if group.entry_mode == ENTRY_MODE_SMART_BUY {
                trade.instruction_data.clone()
            } else {
                Vec::new()
            };
            if group.min_target_buy_lamports() > 0
                && trade.sol_amount_lamports > 0
                && trade.sol_amount_lamports < group.min_target_buy_lamports()
            {
                continue;
            }

            if group.consensus_min_wallets <= 1 {
                if trade.execution_failed {
                    debug!(
                        "Skip single-wallet candidate due to landed failure: [{}] {} | sig: {}..{}",
                        group.name,
                        &token_mint.to_string()[..12],
                        &trade.signature[..8],
                        &trade.signature[trade.signature.len() - 4..],
                    );
                    continue;
                }

                let dedup_key = group_mint_key(&group.id, &token_mint);
                if mint_dedup.contains_key(&dedup_key) {
                    continue;
                }
                mint_dedup.insert(dedup_key, Instant::now());

                let cfg = config.clone();
                let rpc = rpc_client.clone();
                let pf = pumpfun.clone();
                let bh = blockhash_cache.clone();
                let sender = tx_sender.clone();
                let sol = sol_usd.clone();
                let auto_sell = auto_sell_manager.clone();
                let prefetch = prefetch_cache.clone();
                let bc = bc_cache.clone();
                let ata = ata_cache.clone();
                let acct_sub = account_subscriber.clone();
                let tg = tg_notifier.clone();
                let stats = tg_stats.clone();
                let limiter = buy_exec_limiter.clone();
                let bc_fetches = bc_fetches.clone();
                let trade_wallet = trade.source_wallet;
                let group_clone = group.clone();
                let zero_slot_buy_enabled = group_manager.zero_slot_buy_enabled();
                tokio::spawn(async move {
                    let _permit = limiter.acquire_owned().await.expect("buy semaphore closed");
                    execute_buy(
                        &group_clone,
                        &token_mint,
                        &[trade_wallet],
                        trade.detected_at,
                        zero_slot_buy_enabled,
                        &target_instruction_data,
                        &cfg,
                        &rpc,
                        &pf,
                        &bh,
                        &sender,
                        &sol,
                        &auto_sell,
                        &prefetch,
                        &bc,
                        &bc_fetches,
                        &ata,
                        &acct_sub,
                        &tg,
                        &stats,
                    )
                    .await;
                });
            } else {
                if trade.execution_failed {
                    if consensus_engine.reject_signal(
                        &group.id,
                        &token_mint,
                        &trade.source_wallet,
                        &trade.signature,
                    ) {
                        info!(
                            "Consensus candidate rejected: [{}] {} | wallet={}..{} | sig: {}..{}",
                            group.name,
                            &token_mint.to_string()[..12],
                            &trade.source_wallet.to_string()[..4],
                            &trade.source_wallet.to_string()
                                [trade.source_wallet.to_string().len() - 4..],
                            &trade.signature[..8],
                            &trade.signature[trade.signature.len() - 4..],
                        );
                    }
                    continue;
                }

                let buy_signal = BuySignal {
                    group_id: group.id.clone(),
                    group_name: group.name.clone(),
                    token_mint,
                    wallet: trade.source_wallet,
                    token_program,
                    detected_at: trade.detected_at,
                    signature: trade.signature.clone(),
                    consensus_min_wallets: group.consensus_min_wallets,
                    consensus_timeout_secs: group.consensus_timeout_secs,
                    instruction_data: target_instruction_data.clone(),
                    instruction_accounts: trade.instruction_accounts.clone(),
                    sol_amount_lamports: trade.sol_amount_lamports,
                    is_pre_execution: trade.is_pre_execution,
                };
                consensus_engine.submit_signal(buy_signal, &consensus_tx);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_buy(
    group: &CopyGroup,
    mint: &Pubkey,
    wallets: &[Pubkey],
    detected_at: Instant,
    zero_slot_buy_enabled: bool,
    target_instruction_data: &[u8],
    base_config: &AppConfig,
    rpc_client: &Arc<RpcClient>,
    pumpfun: &Arc<PumpfunProcessor>,
    blockhash_cache: &blockhash::BlockhashCache,
    tx_sender: &Arc<TxSender>,
    sol_usd: &SolUsdPrice,
    auto_sell_manager: &Arc<AutoSellManager>,
    prefetch_cache: &Arc<PrefetchCache>,
    bc_cache: &BondingCurveCache,
    bc_fetches: &BondingCurveFetches,
    ata_cache: &AtaBalanceCache,
    _account_subscriber: &Arc<AccountSubscriber>,
    tg: &TgNotifier,
    tg_stats: &Arc<TgStats>,
) {
    let start = Instant::now();
    let detect_to_exec = detected_at.elapsed();
    let mut timings = BuyPathTimings {
        queue: detect_to_exec,
        ..Default::default()
    };
    let config = group.to_app_config(base_config);

    let prefetch_wait_start = Instant::now();
    let prefetched = match prefetch_cache.get(mint) {
        Some(prefetched) => Some(prefetched),
        None => {
            prefetch_cache
                .get_or_wait(mint, Duration::from_millis(PREFETCH_WAIT_MS))
                .await
        }
    };
    timings.prefetch_wait = prefetch_wait_start.elapsed();

    let buy_sol = group.buy_sol_amount;
    let buy_lamports = group.buy_lamports();
    let sol_price = sol_usd.get();
    let has_target_instruction = target_instruction_data.len() >= 24;
    let requires_curve_wait =
        wallets.len() == 1 && pumpfun.target_instruction_requires_curve(target_instruction_data);
    let mut bc_state = bc_cache.get(mint);
    if bc_state.is_none() {
        if let Some(prefetched) = prefetched.as_ref() {
            ensure_bonding_curve_fetch(
                bc_fetches,
                bc_cache,
                pumpfun.clone(),
                *mint,
                prefetched.bonding_curve,
            );
        }
    }
    if bc_state.is_none() {
        let wait_started = Instant::now();
        while wait_started.elapsed() < Duration::from_millis(BC_CACHE_WAIT_MS) {
            if let Some(state) = bc_cache.get(mint) {
                bc_state = Some(state);
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        timings.bc_wait = wait_started.elapsed();
    }
    if bc_state.is_none() && requires_curve_wait {
        if let Some(notify) = bc_fetches.get(mint).map(|entry| entry.clone()) {
            let extra_wait_started = Instant::now();
            let _ = tokio::time::timeout(
                Duration::from_millis(BUY_EXACT_SOL_IN_WAIT_MS),
                notify.notified(),
            )
            .await;
            if let Some(state) = bc_cache.get(mint) {
                bc_state = Some(state);
            }
            timings.bc_wait += extra_wait_started.elapsed();
        }
    }

    let quote_build_start = Instant::now();
    let buy_result: Result<(processor::MirrorInstruction, u64), anyhow::Error> =
        if let Some(ref pf) = prefetched {
            if pf.mirror_accounts.is_empty() {
                Err(anyhow::anyhow!("missing mirror accounts"))
            } else if let Some(bc_state) = bc_state.clone() {
                let token_amount = bc_state.sol_to_token_quote(buy_lamports);
                pumpfun
                    .buy_from_cached_state(
                        mint,
                        &pf.user_ata,
                        &pf.token_program,
                        &pf.source_wallet,
                        &pf.mirror_accounts,
                        &bc_state,
                        &config,
                    )
                    .map(|mirror| (mirror, token_amount))
            } else if has_target_instruction {
                pumpfun.buy_from_target_instruction(
                    mint,
                    &pf.user_ata,
                    &pf.token_program,
                    &pf.source_wallet,
                    &pf.mirror_accounts,
                    target_instruction_data,
                    &config,
                )
            } else {
                Err(anyhow::anyhow!("missing bc cache and target instruction"))
            }
        } else {
            Err(anyhow::anyhow!("prefetch not ready"))
        };
    timings.quote_build = quote_build_start.elapsed();

    let (estimated_tokens_raw, entry_price_sol, entry_mcap_sol) = match &buy_result {
        Ok((_, estimated_tokens)) if *estimated_tokens > 0 => {
            let display_tokens = *estimated_tokens as f64 / 1e6;
            let price = if display_tokens > 0.0 {
                buy_sol / display_tokens
            } else {
                0.0
            };
            let mcap = if let Some(bc_state) = bc_state.clone() {
                bc_state.market_cap_sol()
            } else {
                price * processor::pumpfun::PUMP_TOTAL_SUPPLY
            };
            (*estimated_tokens, price, mcap)
        }
        _ => (0, 0.0, 0.0),
    };

    let entry_price_usd = entry_price_sol * sol_price;
    let entry_mcap_usd = entry_mcap_sol * sol_price;
    let pre_buy_ata_balance = ata_cache.get(mint).unwrap_or(0);

    let mut position = Position::new(
        group.clone(),
        *mint,
        buy_lamports,
        entry_price_sol,
        wallets[0],
        pre_buy_ata_balance,
    );
    position.set_token_amount_estimate(estimated_tokens_raw);
    position.entry_mcap_sol = entry_mcap_sol;
    if let Some(ref pf) = prefetched {
        position.set_sell_snapshot(SellAccountSnapshot {
            bonding_curve: pf.bonding_curve,
            associated_bonding_curve: pf.associated_bonding_curve,
            user_ata: pf.user_ata,
            token_program: pf.token_program,
            mirror_accounts: pf.mirror_accounts.clone(),
            source_wallet: pf.source_wallet,
        });
    }
    let position_key = position.key();

    match buy_result {
        Ok((mirror, _)) => {
            let (blockhash, _) = blockhash_cache.get_sync();
            let tx_build_start = Instant::now();
            let tx_result = if config.jito_enabled {
                let tip = tx_sender.random_jito_tip_account();
                TxBuilder::build_jito_bundle_transaction(
                    &mirror,
                    &config,
                    &config.keypair,
                    blockhash,
                    &tip,
                    group.tip_buy_lamports,
                    &[],
                )
            } else {
                TxBuilder::build_transaction(&mirror, &config, &config.keypair, blockhash, &[])
            };
            timings.tx_build = tx_build_start.elapsed();

            match tx_result {
                Ok(transaction) => {
                    let zero_slot_tx = if zero_slot_buy_enabled && !config.zero_slot_urls.is_empty()
                    {
                        let tip_account = tx_sender.random_0slot_tip_account();
                        Some(TxBuilder::build_0slot_transaction(
                            &mirror,
                            &config,
                            &config.keypair,
                            blockhash,
                            &tip_account,
                            base_config.zero_slot_tip_lamports,
                            &[],
                        ))
                    } else {
                        None
                    };

                    let zero_slot_tx = match zero_slot_tx.transpose() {
                        Ok(tx) => tx,
                        Err(err) => {
                            error!(
                                "Buy 0slot tx build failed [{}] {}: {}",
                                group.name,
                                &mint.to_string()[..12],
                                err
                            );
                            tg_stats.buy_failed.fetch_add(1, Ordering::Relaxed);
                            tg.send(TgEvent::BuyFailed {
                                group_id: group.id.clone(),
                                group_name: group.name.clone(),
                                mint: *mint,
                                reason: format!("0slot tx build failed: {}", err),
                            });
                            return;
                        }
                    };

                    let send_call_start = Instant::now();
                    let send_result = if zero_slot_buy_enabled && !config.zero_slot_urls.is_empty()
                    {
                        tx_sender.fire_and_forget(&transaction, zero_slot_tx.as_ref())
                    } else {
                        tx_sender.fire_and_forget_without_0slot(&transaction)
                    };

                    match send_result {
                        Ok(sig) => {
                            timings.send_call = send_call_start.elapsed();
                            let total_latency = start.elapsed();
                            let sig_str = sig.to_string();
                            let buy_usd = sol_usd.sol_to_usd(buy_sol);

                            info!(
                                "Buy submitted: [{}] {} | {:.4} SOL (${:.2}) | est {:.0} tokens | price={} | mcap={} | queue={} | prefetch={} | bc_wait={} | bc_sync_fetch={} | quote_build={} | tx_build={} | send_call={} | total={} | sig={}",
                                group.name,
                                &mint.to_string()[..12],
                                buy_sol,
                                buy_usd,
                                estimated_tokens_raw as f64 / 1e6,
                                format_price_gmgn(entry_price_usd),
                                format_mcap_usd(entry_mcap_usd),
                                format_latency(timings.queue),
                                format_latency(timings.prefetch_wait),
                                format_latency(timings.bc_wait),
                                format_latency(timings.bc_sync_fetch),
                                format_latency(timings.quote_build),
                                format_latency(timings.tx_build),
                                format_latency(timings.send_call),
                                format_latency(total_latency),
                                &sig_str[..16.min(sig_str.len())],
                            );

                            tg_stats.buy_attempts.fetch_add(1, Ordering::Relaxed);
                            tg.send(TgEvent::BuySubmitted {
                                group_name: group.name.clone(),
                                mint: *mint,
                                sol_amount: buy_sol,
                                latency_ms: total_latency.as_millis() as u64,
                            });

                            if config.auto_sell_enabled {
                                position.mark_submitted(sig_str.clone());
                                position.mark_confirming();
                                auto_sell_manager.add_position(position.clone());

                                let user_ata =
                                    prefetched.as_ref().map(|pf| pf.user_ata).unwrap_or_else(
                                        || get_associated_token_address(&config.pubkey, mint),
                                    );

                                BuyConfirmer::spawn_confirm_task(
                                    rpc_client.clone(),
                                    auto_sell_manager.clone(),
                                    bc_cache.clone(),
                                    ata_cache.clone(),
                                    sol_usd.clone(),
                                    position_key,
                                    group.name.clone(),
                                    *mint,
                                    sig,
                                    config.pubkey,
                                    buy_lamports,
                                    user_ata,
                                    estimated_tokens_raw,
                                    pre_buy_ata_balance,
                                    tg.clone(),
                                );
                            }
                        }
                        Err(err) => {
                            error!(
                                "Buy send failed [{}] {}: {}",
                                group.name,
                                &mint.to_string()[..12],
                                err
                            );
                            tg_stats.buy_failed.fetch_add(1, Ordering::Relaxed);
                            tg.send(TgEvent::BuyFailed {
                                group_id: group.id.clone(),
                                group_name: group.name.clone(),
                                mint: *mint,
                                reason: err.to_string(),
                            });
                        }
                    }
                }
                Err(err) => {
                    error!(
                        "Buy tx build failed [{}] {}: {}",
                        group.name,
                        &mint.to_string()[..12],
                        err
                    );
                    tg_stats.buy_failed.fetch_add(1, Ordering::Relaxed);
                    tg.send(TgEvent::BuyFailed {
                        group_id: group.id.clone(),
                        group_name: group.name.clone(),
                        mint: *mint,
                        reason: format!("buy tx build failed: {}", err),
                    });
                }
            }
        }
        Err(err) => {
            warn!(
                "Buy skipped [{}] {}: {}",
                group.name,
                &mint.to_string()[..12],
                err
            );
        }
    }
}

fn should_skip_signature(sig_cache: &SignatureCache, trade: &DetectedTrade) -> bool {
    let mut entry = sig_cache
        .entry(trade.signature.clone())
        .or_insert(SignatureSeen {
            pre_seen: false,
            landed_seen: false,
            last_seen: Instant::now(),
        });

    entry.last_seen = Instant::now();
    if trade.is_pre_execution {
        if entry.pre_seen || entry.landed_seen {
            return true;
        }
        entry.pre_seen = true;
        false
    } else {
        if entry.landed_seen {
            return true;
        }
        entry.landed_seen = true;
        false
    }
}

fn signal_quality_score(
    has_target_instruction: bool,
    has_instruction_accounts: bool,
    has_sol_amount: bool,
    is_pre_execution: bool,
) -> u32 {
    let mut score = 0u32;
    if has_target_instruction {
        score += 8;
    }
    if has_instruction_accounts {
        score += 4;
    }
    if has_sol_amount {
        score += 2;
    }
    if !is_pre_execution {
        score += 1;
    }
    score
}

fn trade_signal_quality(trade: &DetectedTrade) -> u32 {
    signal_quality_score(
        trade.instruction_data.len() >= 24,
        !trade.instruction_accounts.is_empty(),
        trade.sol_amount_lamports > 0,
        trade.is_pre_execution,
    )
}

fn group_mint_key(group_id: &str, mint: &Pubkey) -> String {
    format!("{}:{}", group_id, mint)
}

fn extract_token_info(trade: &DetectedTrade) -> Option<(Pubkey, Pubkey)> {
    if let Some(mint) = trade.token_mint {
        let token_program = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").ok()?;
        return Some((mint, token_program));
    }

    if trade.instruction_accounts.len() >= 9 {
        let mint = trade.instruction_accounts[2];
        let token_program = trade.instruction_accounts[8];
        if !utils::ata::is_system_address(&mint) {
            return Some((mint, token_program));
        }
    }

    None
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,solana_copy_trader=debug".into()),
        )
        .with_target(false)
        .with_thread_ids(false)
        .with_ansi(true)
        .init();
}
