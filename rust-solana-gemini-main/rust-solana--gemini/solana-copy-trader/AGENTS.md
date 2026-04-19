# AGENTS.md

## Scope

This AGENTS.md applies to this Rust project directory and everything under it.

This directory is the real Rust project root because it contains:
- Cargo.toml
- Cargo.lock
- src/

All instructions below apply to work done in this project tree.

## Project overview

This is a Rust project.

The expected production executable name is:

`copy-trader`

The repository workflow is GitHub-first.
Code changes should be made in GitHub, pushed to `main`, and Linux build artifacts should be produced by GitHub Actions.

I will manually download and run the built artifact on the VPS.
Do not add automatic server deployment unless I explicitly request it.

## Default workflow

Always follow this workflow unless I explicitly say otherwise:

1. Read this `AGENTS.md` first
2. Inspect the relevant project files before making changes
3. Make code changes in this Rust project
4. Push the final changes to the `main` branch
5. Let GitHub Actions automatically build the Linux artifact
6. Ensure the built Linux executable is named `copy-trader`
7. I will manually download the artifact on the VPS
8. I will manually extract, run, or restart it on the VPS

Do not switch to a different workflow by default.

## Environment constraints

- Local development machine: Windows
- Source control and working source of truth: GitHub
- Target runtime environment: Linux VPS
- Project language: Rust
- Production artifact target: Linux
- Expected executable filename: `copy-trader`

Important constraints:

- Do not rely on local Windows compilation as the main production validation method
- Do not assume Windows-built binaries are usable on the Linux VPS
- Treat GitHub Actions Linux builds as the main build source of truth
- Keep runtime instructions compatible with a headless Linux VPS
- Prefer Linux shell commands that can be run over SSH
- Avoid Windows-specific assumptions in paths, shell commands, or deployment instructions

## GitHub workflow rules

When working on this project:

- Prefer direct repository changes intended for GitHub
- The final target branch is `main`
- Keep changes focused and minimal
- If a change is risky or broad, say so clearly before doing it
- Do not rename the production binary unless I explicitly request it
- Do not rename the artifact away from `copy-trader` unless I explicitly request it
- If CI/build files need to be created or updated, do that as part of the task

## Rust project rules

Unless the repository clearly requires something else:

- Use stable Rust
- Prefer `cargo fmt`
- Prefer `cargo clippy --all-targets --all-features -- -D warnings` when reasonable
- Use `cargo build --release` for production builds
- Keep dependencies minimal
- Avoid unnecessary refactors
- Preserve the existing crate structure unless there is a strong reason to change it
- Prefer small, reviewable changes over large rewrites

## Build output requirements

The build pipeline must produce a Linux artifact that contains the executable:

`copy-trader`

Expected assumptions:

- The runtime target is Linux
- The output must be suitable for manual download on a VPS
- Packaging may be `.tar.gz` or another simple Linux-friendly archive format
- Inside the package, the executable itself must be named `copy-trader`

If build or packaging logic changes, preserve this naming contract.

## GitHub Actions requirements

If GitHub Actions is missing, incomplete, or broken, help create or fix it.

Preferred GitHub Actions behavior:

1. Trigger on push to `main`
2. Build on Ubuntu
3. Run formatting checks when appropriate
4. Run clippy when appropriate
5. Build the project in release mode
6. Package the Linux binary
7. Upload a downloadable artifact
8. Ensure the packaged executable is named `copy-trader`

Do not add automatic VPS deployment unless I explicitly ask for it.

## VPS deployment boundary

I will manually handle VPS deployment.

That means:

- I will manually download the built artifact on the VPS
- I will manually extract it
- I will manually run or restart the executable

So when you finish a task, do not claim the VPS has already been updated unless I explicitly confirm it.

Do not add:
- auto-deploy to VPS
- SSH deployment steps in CI
- remote restart hooks
- deployment secrets for server access

Unless I explicitly request those later.

## Binary naming rules

The final Linux executable name must be:

`copy-trader`

If there is also a packaged archive, prefer a clear name such as:

`copy-trader-linux-x86_64.tar.gz`

But inside the package, the executable itself must still be:

`copy-trader`

Do not introduce mismatched names between:
- Cargo output
- packaged artifact
- VPS run commands
- documentation
- CI steps

If the repository currently produces a different binary name, fix the workflow and explain the change clearly.

## What to do before making changes

Before major edits:

1. Read this file
2. Inspect the relevant files
3. Identify whether code, Cargo config, CI, packaging, or runtime instructions will be affected
4. Briefly state the plan
5. Then make changes

For small changes, keep the plan short and practical.

## What to do after making changes

After finishing a task, always provide:

1. A short summary of what changed
2. A list of files changed
3. Any Cargo or dependency changes
4. Any GitHub Actions or CI changes
5. Confirmation of the expected artifact name
6. Exact manual VPS steps to:
   - download the artifact
   - extract it
   - make it executable if needed
   - run or restart it
7. Rollback steps
8. Any risks, assumptions, or follow-up checks

Always include concrete commands where possible.

## Long task behavior

If the task is long, spans multiple steps, or the conversation becomes large:

- Update `SESSION_SUMMARY.md`

That summary should include:
- current goal
- current progress
- files changed
- CI / GitHub Actions status
- artifact/package naming
- manual VPS run steps
- remaining work
- exact next step for the next thread

When starting a new thread:

1. Read `AGENTS.md`
2. Read `SESSION_SUMMARY.md` if it exists
3. Restate the remaining work
4. Continue without redoing completed work

## What to avoid

Do not:

- rely on Windows-local compilation as production validation
- assume Windows paths or Windows-only shell behavior in production instructions
- stop at “code updated” without covering CI/build implications
- rename the binary away from `copy-trader`
- add automatic VPS deployment unless explicitly asked
- silently change build targets, toolchains, or artifact names
- make large unrelated refactors during a focused task
- claim the build is deploy-ready without considering Linux artifact generation

## Preferred response style

Be concrete, operational, and explicit.

Prefer:
- copy-paste-ready commands
- short plans before edits
- exact filenames
- exact artifact names
- exact Linux shell steps
- explicit mention of changed files

Avoid vague summaries when a concrete command or filename would be better.

## Standard completion template

When finishing work, prefer using this structure:

### Summary
- what changed

### Files changed
- file1
- file2

### CI / GitHub Actions
- what changed in CI
- what artifact is expected

### Artifact
- executable name: `copy-trader`
- package name: `copy-trader-linux-x86_64.tar.gz` (or the current actual package name)

### VPS manual steps
```bash
# download artifact
# extract package
# chmod +x copy-trader
# run or restart
