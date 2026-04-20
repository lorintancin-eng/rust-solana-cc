# Session Summary

## Current Goal
- Keep the Pump.fun mirror fast path for safe direct buys.
- Detect unsafe direct mirror account layouts and fall back to native Pump.fun account construction.
- Prevent direct buys from failing on-chain with Pump.fun `ConstraintSeeds (2006)` errors such as `user_volume_accumulator`.

## Current Progress
- Added canonical Pump.fun buy-account derivation in `src/processor/pumpfun.rs`.
- Added direct mirror account validation:
  - exact account-count check for direct mirror buys
  - full canonical account comparison when bonding-curve state is cached
  - partial fixed-slot validation when only the target instruction is available
- Updated buy routing in `src/main.rs`:
  - safe direct mirror buys keep using the existing mirror fast path
  - unsafe direct mirror buys fall back to `buy_standard_from_cached_state(...)` when bonding-curve cache exists
  - unsafe direct mirror buys are rejected when there is no bonding-curve cache to build a safe native instruction
- Propagated `TradeOrigin` into `execute_buy()` so only direct Pump.fun mirror buys are gated by the new safety check.
- Bumped crate version from `1.6.60` to `1.6.61`.
- Per user instruction, no local edit-level compile/build checks were run.

## Files Changed
- `Cargo.toml`
- `Cargo.lock`
- `SESSION_SUMMARY.md`
- `src/main.rs`
- `src/processor/pumpfun.rs`

## CI / GitHub Actions Status
- Workflow: `.github/workflows/build-copy-trader-linux.yml`
- Trigger: push to `main`
- Build environment: Ubuntu
- Validation source of truth: GitHub Actions Linux build only

## Artifact / Package Naming
- Executable name: `copy-trader`
- Artifact name: `copy-trader-linux`
- Downloaded contents: raw executable `copy-trader`

## Manual VPS Run Steps
```bash
rm -rf /tmp/build && mkdir -p /tmp/build
gh run list --repo lorintancin-eng/rust-solana-cc --workflow "Build Copy Trader Linux" --branch main --limit 5
gh run download <RUN_ID> --repo lorintancin-eng/rust-solana-cc --name copy-trader-linux -D /tmp/build
chmod +x /tmp/build/copy-trader
cp /tmp/build/copy-trader /home/ubuntu/rust_project/copy-trader
pkill -f copy-trader || true
nohup /home/ubuntu/rust_project/copy-trader > /home/ubuntu/rust_project/copy-trader.log 2>&1 &
```

## Remaining Work
- Commit the direct mirror safety / native fallback changes
- Push to `main`
- Check the latest GitHub Actions Linux build result

## Exact Next Step For Next Thread
- Read `AGENTS.md`
- Read this `SESSION_SUMMARY.md`
- Check the latest `main` commit and Linux Actions run
- If the Linux build is green, download artifact `copy-trader-linux` on VPS and restart manually
