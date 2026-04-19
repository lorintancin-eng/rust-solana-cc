# Session Summary

## Current Goal
- Fix gRPC account-stream overflow after sells.
- Prevent stale candidate-account subscriptions from growing without bound.

## Current Progress
- Raised the account-stream gRPC decoding limit to `64MB`, matching the trade stream.
- Added stale candidate mint tracking and pruning in `AccountSubscriber`.
- Added periodic candidate cleanup in `main`, preserving mints that still have non-closed positions.
- Added `AutoSellManager::get_open_position_mints()` to protect submitted / confirming / selling / active positions from cleanup.
- Bumped crate version from `1.6.59` to `1.6.60`.
- Per user instruction, no local edit-level compile/build checks were run.

## Files Changed
- `Cargo.toml`
- `Cargo.lock`
- `SESSION_SUMMARY.md`
- `src/grpc/account_subscriber.rs`
- `src/autosell/manager.rs`
- `src/main.rs`

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
- Commit current account-stream fixes
- Push to `main`
- Check the latest GitHub Actions Linux build result

## Exact Next Step For Next Thread
- Read `AGENTS.md`
- Read this `SESSION_SUMMARY.md`
- Check latest `main` commit and Linux Actions run
- If build is green, download artifact `copy-trader-linux` on VPS and restart manually
