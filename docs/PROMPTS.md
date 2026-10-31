# Prompt Log

All prompts and responses for this project, oldest first. Times are local (UTC-03).

## 2026-10-31 16:36:14 -03

### Prompt

```
Before we begin:

1. We will adhere to the rules in CLAUDE.md.

2. We will log all prompts and responses in docs/PROMPTS.md with date and time stamps.
```

### Response

Agreed on both. CLAUDE.md applies to all work here, and this log now exists at docs/PROMPTS.md with this exchange as its first entry. A memory note carries the logging rule into future sessions.

- Risk: .env is untracked and the repo has no .gitignore, so `git add .` would commit it. Nothing was committed or changed for this.

## 2026-10-31 16:39:03 -03

### Prompt

```
This Rust project is a pump.fun transaction indexer based on Solana full blocks. Its WSS endpoint for blockSubscribe and RPC for recovering lost blocks are configured in the .env file.

Create a Rust workspace in this folder, add an appropriate .gitignore file, and make the first commit to main.
```

### Response

Created a virtual workspace (edition 2024, resolver 3) with one stub binary crate, `pump-fun-indexer`, in crates/indexer, because a workspace needs at least one member to build. The stub only prints its name and has no dependencies. The .gitignore covers /target and .env.

Verified: `cargo run`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` all pass, and `git check-ignore` confirms .env and target are ignored.

Committed to main, including CLAUDE.md and docs/PROMPTS.md. Not pushed.

- Note: main already had commit 23fbdd1 (README only), so this commit sits on top of it rather than being the first commit in the repo.
