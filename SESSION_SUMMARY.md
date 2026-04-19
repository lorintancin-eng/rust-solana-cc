# Session Summary

## Current Goal
- Add wrapper-wallet copy support without regressing the existing direct Pump.fun fast path.

## Current Progress
- Created and pushed rollback tag:
  - `rollback/pre-wrapper-native-buy-1.6.58`
  - points to commit `dd0d1a1`
- Implemented wrapper-side buy isolation:
  - direct Pump.fun instructions still use the existing mirror-account fast path
  - wrapper CPI pre-exec signals now stop writing fake `mirror_accounts`
  - wrapper signals now carry inferred `token_program`
  - wrapper buys fall back to a native Pump.fun account builder using `mint + token_program + bonding curve state + PDA derivation`
- Added wrapper execution isolation:
  - direct buy executor concurrency remains `8`
  - wrapper buy executor concurrency is separated and limited to `2`
- Added sell fallback for wrapper-opened positions:
  - if a position has no real `mirror_accounts`, autosell now routes through `sell_standard(...)`
- Bumped crate version from `1.6.58` to `1.6.59`
- Per user instruction, no local edit-level compile/build checks were run

## Files Changed
- `Cargo.toml`
- `Cargo.lock`
- `SESSION_SUMMARY.md`
- `src/processor/mod.rs`
- `src/grpc/subscriber.rs`
- `src/consensus/engine.rs`
- `src/processor/pumpfun.rs`
- `src/main.rs`
- `src/tx/sell_executor.rs`

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
- Commit current wrapper-support changes
- Push to `main`
- Wait for GitHub Actions Linux build result

## Exact Next Step For Next Thread
- Read `AGENTS.md`
- Read this `SESSION_SUMMARY.md`
- Check the latest `main` commit and Linux Actions run
- If build is green, download artifact `copy-trader-linux` on VPS and restart manually
