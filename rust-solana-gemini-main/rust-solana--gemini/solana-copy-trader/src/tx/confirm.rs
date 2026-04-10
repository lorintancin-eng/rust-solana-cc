use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::autosell::AutoSellManager;
use crate::grpc::{AtaBalanceCache, BondingCurveCache};
use crate::telegram::{TgEvent, TgNotifier};
use crate::utils::sol_price::SolUsdPrice;

pub struct BuyConfirmer;

const BUY_CONFIRM_FAST_POLL_MS: u64 = 25;
const BUY_CONFIRM_SLOW_POLL_MS: u64 = 80;
const BUY_CONFIRM_FAST_WINDOW_MS: u64 = 400;
const ATA_RENT: u64 = 2_039_280;

pub fn format_price_gmgn(usd: f64) -> String {
    if usd <= 0.0 {
        return "$0".to_string();
    }
    if usd >= 0.01 {
        return format!("${:.4}", usd);
    }

    let s = format!("{:.15}", usd);
    if let Some(dot_pos) = s.find('.') {
        let decimals = &s[dot_pos + 1..];
        let leading_zeros = decimals.chars().take_while(|&c| c == '0').count();
        if leading_zeros > 0 {
            let significant = &decimals[leading_zeros..];
            let sig_digits: String = significant.chars().take(4).collect();
            return format!("$0.0{}{}", to_subscript(leading_zeros), sig_digits);
        }
    }

    format!("${:.6}", usd)
}

pub fn format_mcap_usd(usd: f64) -> String {
    if usd >= 1_000_000.0 {
        format!("${:.2}M", usd / 1_000_000.0)
    } else if usd >= 1_000.0 {
        format!("${:.2}K", usd / 1_000.0)
    } else {
        format!("${:.0}", usd)
    }
}

fn to_subscript(n: usize) -> String {
    let subscripts = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
    if n < 10 {
        subscripts[n].to_string()
    } else {
        n.to_string()
            .chars()
            .map(|c| subscripts[c.to_digit(10).unwrap_or(0) as usize])
            .collect()
    }
}

impl BuyConfirmer {
    fn poll_interval(elapsed: Duration) -> Duration {
        if elapsed.as_millis() < BUY_CONFIRM_FAST_WINDOW_MS as u128 {
            Duration::from_millis(BUY_CONFIRM_FAST_POLL_MS)
        } else {
            Duration::from_millis(BUY_CONFIRM_SLOW_POLL_MS)
        }
    }

    pub fn spawn_confirm_task(
        rpc_client: Arc<RpcClient>,
        auto_sell: Arc<AutoSellManager>,
        bc_cache: BondingCurveCache,
        ata_cache: AtaBalanceCache,
        sol_usd: SolUsdPrice,
        mint: Pubkey,
        signature: Signature,
        user_pubkey: Pubkey,
        entry_sol_amount: u64,
        user_ata: Pubkey,
        estimated_tokens_raw: u64,
        tg: TgNotifier,
    ) {
        tokio::spawn(async move {
            let start = Instant::now();
            let mint_short = &mint.to_string()[..12];
            let buy_sol = entry_sol_amount as f64 / 1e9;

            info!(
                "链上确认启动: {} | sig: {}",
                mint_short,
                &signature.to_string()[..16],
            );

            let mut confirmed = false;
            let mut token_balance = 0u64;
            let max_wait = Duration::from_secs(10);

            while start.elapsed() < max_wait {
                if token_balance == 0 {
                    if let Some(cached_balance) = ata_cache.get(&mint) {
                        if cached_balance > 0 {
                            token_balance = cached_balance;
                            info!(
                                "链上确认: {} gRPC 缓存命中 ATA 余额: {} | {:.0}ms",
                                mint_short,
                                cached_balance,
                                start.elapsed().as_millis(),
                            );
                        }
                    }
                }

                let status_task = if confirmed {
                    None
                } else {
                    let rpc = rpc_client.clone();
                    let sig = signature;
                    Some(tokio::task::spawn_blocking(move || {
                        rpc.get_signature_statuses(&[sig])
                    }))
                };

                let ata_task = if token_balance > 0 {
                    None
                } else {
                    let rpc = rpc_client.clone();
                    let ata = user_ata;
                    Some(tokio::task::spawn_blocking(move || {
                        rpc.get_token_account_balance(&ata)
                    }))
                };

                if let Some(status_task) = status_task {
                    match status_task.await {
                        Ok(Ok(response)) => {
                            if let Some(Some(status)) = response.value.first() {
                                if status.err.is_some() {
                                    warn!(
                                        "链上确认: {} 交易失败 | err: {:?} | {:.0}ms",
                                        mint_short,
                                        status.err,
                                        start.elapsed().as_millis(),
                                    );
                                    auto_sell.confirm_failed(&mint, "tx failed on-chain");
                                    tg.send(TgEvent::BuyFailed {
                                        mint,
                                        reason: format!("链上交易失败: {:?}", status.err),
                                    });
                                    return;
                                }
                                confirmed = true;
                                debug!(
                                    "链上确认: {} 签名已确认 | {:.0}ms",
                                    mint_short,
                                    start.elapsed().as_millis(),
                                );
                            }
                        }
                        Ok(Err(e)) => {
                            debug!("链上确认 RPC 错误: {} ({})", e, mint_short);
                        }
                        Err(e) => {
                            debug!("链上确认任务错误: {} ({})", e, mint_short);
                        }
                    }
                }

                if token_balance == 0 {
                    if let Some(cached_balance) = ata_cache.get(&mint) {
                        if cached_balance > 0 {
                            token_balance = cached_balance;
                            info!(
                                "链上确认: {} gRPC 缓存命中 ATA 余额: {} | {:.0}ms",
                                mint_short,
                                cached_balance,
                                start.elapsed().as_millis(),
                            );
                        }
                    }
                }

                if token_balance == 0 {
                    if let Some(ata_task) = ata_task {
                        match ata_task.await {
                            Ok(Ok(balance)) => {
                                let parsed = balance.amount.parse::<u64>().unwrap_or(0);
                                if parsed > 0 {
                                    token_balance = parsed;
                                    info!(
                                        "链上确认: {} RPC ATA 余额: {} | {:.0}ms",
                                        mint_short,
                                        parsed,
                                        start.elapsed().as_millis(),
                                    );
                                }
                            }
                            Ok(Err(e)) => {
                                debug!(
                                    "链上确认: {} ATA RPC 查询失败: {} | {:.0}ms",
                                    mint_short,
                                    e,
                                    start.elapsed().as_millis(),
                                );
                            }
                            Err(e) => {
                                debug!("链上确认: {} ATA 任务错误: {}", mint_short, e);
                            }
                        }
                    }
                }

                if token_balance > 0 {
                    confirmed = true;
                    break;
                }

                tokio::time::sleep(Self::poll_interval(start.elapsed())).await;
            }

            if confirmed && token_balance == 0 {
                warn!(
                    "链上确认: {} 签名已确认但 ATA 余额为 0 | {:.0}ms | 尝试 getTransaction",
                    mint_short,
                    start.elapsed().as_millis(),
                );

                let rpc2 = rpc_client.clone();
                let sig2 = signature;
                let tx_detail = tokio::task::spawn_blocking(move || {
                    rpc2.get_transaction(
                        &sig2,
                        solana_transaction_status::UiTransactionEncoding::JsonParsed,
                    )
                })
                .await;

                match tx_detail {
                    Ok(Ok(tx)) => {
                        if let Some(meta) = &tx.transaction.meta {
                            use solana_transaction_status::option_serializer::OptionSerializer;
                            if let OptionSerializer::Some(ref token_balances) =
                                meta.post_token_balances
                            {
                                let user_str = user_pubkey.to_string();
                                for tb in token_balances.iter() {
                                    let owner_match = match &tb.owner {
                                        OptionSerializer::Some(owner) => owner == &user_str,
                                        _ => false,
                                    };
                                    if owner_match {
                                        if let Ok(amount) = tb.ui_token_amount.amount.parse::<u64>()
                                        {
                                            if amount > 0 {
                                                token_balance = amount;
                                                info!(
                                                    "链上确认: {} 从交易详情提取余额: {} | {:.0}ms",
                                                    mint_short,
                                                    token_balance,
                                                    start.elapsed().as_millis(),
                                                );
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(Err(e)) => warn!("链上确认: {} getTransaction 失败: {}", mint_short, e),
                    Err(e) => warn!("链上确认: {} getTransaction 任务错误: {}", mint_short, e),
                }
            }

            if token_balance > 0 {
                let display_tokens = token_balance as f64 / 1e6;
                let bc_price = bc_cache
                    .get(&mint)
                    .map(|s| s.price_sol())
                    .filter(|p| *p > 0.0);
                let current_market_price = bc_price.unwrap_or(0.0);

                let entry_price = if let Some(p) = bc_price {
                    p
                } else if display_tokens > 0.0 {
                    buy_sol / display_tokens
                } else {
                    0.0
                };

                let estimated_display = estimated_tokens_raw as f64 / 1e6;
                let slippage_pct = if estimated_display > 0.0 {
                    (estimated_display - display_tokens) / estimated_display * 100.0
                } else {
                    0.0
                };

                let pnl = if entry_price > 0.0 && current_market_price > 0.0 {
                    ((current_market_price - entry_price) / entry_price) * 100.0
                } else {
                    0.0
                };

                let sol_price_usd = sol_usd.get();
                let value_sol = buy_sol * (1.0 + pnl / 100.0);
                let value_usd = value_sol * sol_price_usd;
                let cost_usd = format_price_gmgn(entry_price * sol_price_usd);

                let mcap_sol = current_market_price * crate::processor::pumpfun::PUMP_TOTAL_SUPPLY;
                let mcap_usd = mcap_sol * sol_price_usd;
                let entry_price_for_position = if entry_price > 0.0 {
                    Some(entry_price)
                } else {
                    bc_price
                };

                auto_sell.confirm_success(&mint, token_balance, entry_price_for_position);

                let mcap_str = format_mcap_usd(mcap_usd);
                let rpc_info = rpc_client.clone();
                let auto_sell_info = auto_sell.clone();
                let mint_info = mint;
                let entry_mcap_sol_val = mcap_sol;
                tokio::spawn(async move {
                    let ti =
                        crate::utils::token_info::fetch_token_info(&rpc_info, &mint_info).await;
                    let name = if ti.name.is_empty() {
                        let ms = mint_info.to_string();
                        format!("{}..{}", &ms[..6], &ms[ms.len() - 4..])
                    } else {
                        ti.name.clone()
                    };
                    auto_sell_info.update_token_info(&mint_info, name, entry_mcap_sol_val);
                });

                let token_name_short = {
                    let ms = mint.to_string();
                    format!("{}..{}", &ms[..6], &ms[ms.len() - 4..])
                };

                info!(
                    "✅ 持仓确认: {} | {:.0} tokens | 成本: {} | 市值: {} | PnL: {:.2}% | 滑点: {:.1}% | 价值: {:.4} SOL (${:.2}) | {:.0}ms",
                    mint_short,
                    display_tokens,
                    cost_usd,
                    format_mcap_usd(mcap_usd),
                    pnl,
                    slippage_pct,
                    value_sol,
                    value_usd,
                    start.elapsed().as_millis(),
                );

                tg.send(TgEvent::BuyConfirmed {
                    mint,
                    token_name: token_name_short,
                    spent_sol: buy_sol,
                    cost_price_usd: cost_usd.clone(),
                    mcap_usd: mcap_str,
                });

                let rpc_bg = rpc_client.clone();
                let auto_sell_bg = auto_sell.clone();
                let display_tokens_bg = display_tokens;
                tokio::spawn(async move {
                    if let Some(actual_sol) =
                        Self::get_actual_sol_spent(&rpc_bg, signature, &user_pubkey).await
                    {
                        let real_price = if display_tokens_bg > 0.0 {
                            actual_sol / display_tokens_bg
                        } else {
                            0.0
                        };
                        if real_price > 0.0 {
                            auto_sell_bg.update_entry_price(&mint, real_price);
                            debug!(
                                "入场价修正: {} | {:.10} SOL/token",
                                &mint.to_string()[..12],
                                real_price,
                            );
                        }
                    }
                });
            } else if confirmed {
                warn!(
                    "链上确认: {} 交易已确认但余额为 0 → 删除仓位 | {:.0}ms",
                    mint_short,
                    start.elapsed().as_millis(),
                );
                auto_sell.confirm_failed(&mint, "confirmed but zero balance");
                tg.send(TgEvent::BuyFailed {
                    mint,
                    reason: "交易已确认但余额为 0".to_string(),
                });
            } else {
                warn!(
                    "链上确认: {} 超时且余额为 0 → 删除仓位 | {:.0}ms",
                    mint_short,
                    start.elapsed().as_millis(),
                );
                auto_sell.confirm_failed(&mint, "timeout with zero balance");
                tg.send(TgEvent::BuyFailed {
                    mint,
                    reason: "确认超时且余额为 0".to_string(),
                });
            }
        });
    }

    async fn get_actual_sol_spent(
        rpc_client: &Arc<RpcClient>,
        signature: Signature,
        user_pubkey: &Pubkey,
    ) -> Option<f64> {
        let rpc = rpc_client.clone();
        let sig = signature;
        let user = *user_pubkey;

        let result = tokio::task::spawn_blocking(move || {
            rpc.get_transaction(&sig, solana_transaction_status::UiTransactionEncoding::Json)
        })
        .await;

        match result {
            Ok(Ok(tx)) => {
                let meta = tx.transaction.meta.as_ref()?;
                let account_keys: Vec<String> = match &tx.transaction.transaction {
                    solana_transaction_status::EncodedTransaction::Json(ui_tx) => {
                        match &ui_tx.message {
                            solana_transaction_status::UiMessage::Raw(msg) => {
                                msg.account_keys.clone()
                            }
                            solana_transaction_status::UiMessage::Parsed(msg) => {
                                msg.account_keys.iter().map(|k| k.pubkey.clone()).collect()
                            }
                        }
                    }
                    _ => return None,
                };

                let user_str = user.to_string();
                let user_idx = account_keys.iter().position(|k| k == &user_str)?;

                let pre_balance = meta.pre_balances.get(user_idx)?;
                let post_balance = meta.post_balances.get(user_idx)?;

                if pre_balance > post_balance {
                    let total_spent = pre_balance - post_balance;
                    let fee = meta.fee;
                    let mut deductions = fee;

                    use solana_transaction_status::option_serializer::OptionSerializer;
                    let has_new_ata = if let (
                        OptionSerializer::Some(ref pre_tb),
                        OptionSerializer::Some(ref post_tb),
                    ) = (&meta.pre_token_balances, &meta.post_token_balances)
                    {
                        let user_str = user.to_string();
                        let pre_has_user = pre_tb.iter().any(|tb| {
                            matches!(&tb.owner, OptionSerializer::Some(o) if o == &user_str)
                        });
                        let post_has_user = post_tb.iter().any(|tb| {
                            matches!(&tb.owner, OptionSerializer::Some(o) if o == &user_str)
                        });
                        !pre_has_user && post_has_user
                    } else {
                        false
                    };

                    if has_new_ata {
                        deductions += ATA_RENT;
                    }

                    let token_cost = total_spent.saturating_sub(deductions);
                    let sol = token_cost as f64 / 1e9;
                    debug!(
                        "实际 SOL 花费: {:.6} SOL (total={}, fee={}, ata_rent={}, token_cost={})",
                        sol,
                        total_spent,
                        fee,
                        if has_new_ata { ATA_RENT } else { 0 },
                        token_cost,
                    );
                    Some(sol)
                } else {
                    None
                }
            }
            Ok(Err(e)) => {
                debug!("getTransaction 失败: {}", e);
                None
            }
            Err(e) => {
                debug!("getTransaction 任务错误: {}", e);
                None
            }
        }
    }
}
