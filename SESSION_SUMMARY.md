# Session Summary

## Current Goal
- Complete a full code-level understanding of this Rust copy-trading project, including module responsibilities, main runtime flow, config dependencies, CI artifact rules, and current limitations.

## Current Progress
- Read all relevant project files:
  - `Cargo.toml`
  - `.github/workflows/build-copy-trader-linux.yml`
  - all source files under `src/`
- Confirmed the main entrypoint is `src/main.rs`.
- Confirmed the production binary name is `copy-trader`.
- Completed walkthrough of:
  - gRPC trade subscription and parsing
  - group management and consensus triggering
  - buy instruction construction and transaction sending
  - buy confirmation and position lifecycle
  - auto-sell and Jupiter fallback
  - Telegram control plane and performance reporting

## Files Changed
- `SESSION_SUMMARY.md`

## CI / GitHub Actions Status
- Current workflow file: `.github/workflows/build-copy-trader-linux.yml`
- Trigger: push to `main` or manual dispatch
- Build environment: Ubuntu
- Build flow:
  - `cargo build --locked --release --bin copy-trader`
  - package as `copy-trader-linux-x86_64.tar.gz`
  - upload artifact `copy-trader-linux-x86_64`

## Artifact / Package Naming
- Executable name: `copy-trader`
- Archive name: `copy-trader-linux-x86_64.tar.gz`
- Executable inside archive: `copy-trader`

## Manual VPS Run Steps
```bash
# list recent runs
gh run list --repo lorintancin-eng/rust-solana-cc --workflow "Build Copy Trader Linux" --branch main --limit 5

# download artifact
gh run download <RUN_ID> --repo lorintancin-eng/rust-solana-cc --name copy-trader-linux-x86_64

# extract
tar -xzf copy-trader-linux-x86_64.tar.gz

# allow execute
chmod +x copy-trader

# restart
pkill -f copy-trader || true
nohup ./copy-trader > copy-trader.log 2>&1 &
```

## Remaining Work
- The "understand the whole project" goal is complete for this session.
- Optional next work:
  - produce a full sequence diagram
  - inspect and fix multi-DEX wiring gaps
  - inspect why position recovery loading is not wired into startup

## Important Findings
- The live buy path is optimized most deeply for Pump.fun.
- `src/grpc/subscriber.rs` contains direction parsing for PumpSwap and Raydium, but `try_parse_instruction()` currently only accepts Pump.fun trades into the main execution path.
- `src/processor/` already contains builders for PumpSwap, Raydium AMM, and Raydium CPMM, so the architecture is prepared for multi-DEX support, but parsing and execution are not fully wired through.
- `src/autosell/persistence.rs` can save and load `positions.json`, but `load_positions()` is not currently connected to startup recovery.
- Runtime stats and group performance updates are mainly recorded through the Telegram event path; if Telegram is disabled, those stats are not fully populated.

## Exact Next Step For Next Thread
- Read `AGENTS.md`.
- Read this `SESSION_SUMMARY.md`.
- Then continue with one of:
  - "draw the full runtime sequence"
  - "fix non-Pump.fun DEX wiring"
  - "wire position recovery into startup"
