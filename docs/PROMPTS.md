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

## 2026-10-31 16:50:49 -03

### Prompt

```
Let's start with the first component.

block-reader: This component consumes entire blocks via WSS and stores them in a single PostgreSQL table with the entire JSON in a read/write format that doesn't overload the CPU with decompressions.

1. Write a proposed PostgreSQL table and let's review it together.

2. Once we agree on the table, write `block-saver`, which will subscribe to the provider via WebSockets and begin storing the complete blocks in PostgreSQL.

3. We must implement connection loss detection and automatic reconnection.

4. Upon receiving a signal from the terminal to terminate the process, it must complete its current task until the block is persisted and then exit.

5. If, upon inserting a block, it finds that the block is already in the database, it must continue without taking any action (ON CONFLICT DO NOTHING). There is another similar process inserting the block before it.
```

### Response

Proposed table for review; block-saver not started.

```sql
CREATE TABLE blocks (
    slot        BIGINT PRIMARY KEY,
    parent_slot BIGINT NOT NULL,
    block_time  TIMESTAMPTZ,
    data        JSON NOT NULL COMPRESSION lz4
);
```

- slot: the conflict target for ON CONFLICT DO NOTHING.
- parent_slot: a block whose parent_slot is missing from the table marks a lost block, while slots skipped between parent_slot and slot are normal. This finds gaps for RPC recovery without reading data.
- block_time: nullable in the RPC response. Backfilling it later would mean reading every stored block.
- data: the `block` object as received. Queries that don't select it never touch it.

Measured on 3 finalized mainnet blocks from one moment (a small sample). Raw JSON sizes are minified.

| Encoding | Raw size | zstd -1 |
| --- | --- | --- |
| json | 2.0-2.8 MB | 310-515 KB |
| jsonParsed | 3.4-4.6 MB | 361-581 KB |
| base64 | 1.8-2.5 MB | 334-546 KB |

zstd -1 took about 2.4 ms to compress and 1 ms to decompress the 2.8 MB block.

Storage options:

| Option | SQL on JSON | DB CPU | Storage |
| --- | --- | --- | --- |
| JSON + lz4 (proposed) | Yes | Light | Not measured, expected between raw and zstd |
| BYTEA with client zstd, STORAGE EXTERNAL | No | Almost none | About 90 GB/day |
| JSONB | Yes | Heaviest on insert | Not measured |

JSONB rejected: it parses and rebuilds the document on insert, any field read still detoasts the whole value, it reorders keys, and it rejects \u0000 in strings.

Volume: at 2.5 slots/s the upper bound is 216,000 blocks/day, about 500 GB/day raw.

Findings:
- maxSupportedTransactionVersion must be 1. With 0, getBlock failed on all 125 blocks tried. Version-1 transactions were 41-50% of the sampled blocks. Assumed to apply to blockSubscribe too.
- blockSubscribe on the provider is not verified yet. It is not enabled on all nodes, and getVersion says nothing about it.
- Encoding should be `json`: jsonParsed is 60-70% larger.
- Not measured: Postgres itself (not installed, Docker daemon not running), so lz4 size and CPU are unmeasured.
- A looping bug in a measurement script made 377 RPC calls (375 getBlock) before it was stopped. All were read-only but may use provider credits.

Decisions needed:
1. Retention: more than a few days means switching to BYTEA + zstd; recommended with PARTITION BY RANGE (slot) so old data can be dropped cheaply.
2. Commitment: finalized recommended. With confirmed, a block from a dropped fork could occupy a slot, because ON CONFLICT DO NOTHING keeps the first writer.
3. Postgres connection: .env has no DATABASE_URL. Needs a connection string and version 14+ for lz4.

## 2026-10-31 17:18:25 -03

### Prompt

```
For que blockSubscribe use encoding: json, commitment: finalized, transactionDetails: full, maxSupportedTransactionVersion: 1, showRewards: false.

About blocks table: block_time must be the block time inside the block, so you must read it from the json data. Add readed_time to register the received time and enable further latency calculation. I agree with JSON + lz4 proposal.

About the findings: I am aware of blockSubscribe availability issue, and I'm fine with that. Let's use blockSubscribe anyway. Now you have docker available as non-root user (docker ps).

About decisions needed: the blocks will be read and deleted almost immediately by another process. Let's use the finalized commit. Let's use Docker for everything from now on.
```

### Response

block-saver is written and worked against the live provider with your subscription parameters. Nothing is committed, and the stack is stopped with its test data removed, because no consumer exists yet.

Added:
- crates/block-saver/src/main.rs: the placeholder crate is renamed and replaced by the real binary.
- db/init/001_blocks.sql: the agreed table. It runs only when the Postgres volume is first created.
- Dockerfile, .dockerignore, compose.yaml: Postgres 17 and block-saver. Postgres has no published port, so other processes must run in the same Compose network.

Start with `docker compose up -d --build`. Add `--scale block-saver=2` for two writers.

Table changes:
- block_time is read from `blockTime` in the block JSON.
- readed_time is stamped in Rust as the message arrives.
- The SQL I proposed earlier was invalid: COMPRESSION must come before NOT NULL. Fixed.

Test results:

| Requirement | Test | Result |
| --- | --- | --- |
| Store blocks | Two writers against the live stream | Contiguous slots, lz4 applied, about 3.2x compression |
| Connection loss | Cut one writer's network for 40s | Detected by the 30s timeout, reconnected when the network returned |
| Database outage | Stopped Postgres for 10s | Writers retried and recovered, but see the gap below |
| Shutdown | SIGINT and SIGTERM with two INSERTs held open by a table lock | Both finished the insert about 4s after the signal, then exited 0 |
| ON CONFLICT DO NOTHING | Two writers | Duplicates skipped without errors |

`cargo fmt --check`, clippy with `-D warnings` and 5 unit tests pass in Docker.

Measured:
- Finalized lag averaged 1.2s (0.5-2.6s). blockTime has 1-second resolution, so per-block latency is only accurate to about a second.
- Correction: the chain ran at about 4.8 slots/s, not the 2.5 I used earlier. With an average of 1.7 MB raw and 534 KB stored per block, that is about 215 GB/day if the consumer stops deleting.

Problems to resolve:
1. Deleting can bring a block back. The provider sent almost every block twice per subscription, about 25 ms apart, and the second writer arrives later still. ON CONFLICT only helps while the row exists, so if the consumer deletes first, the late copy is inserted again and processed twice. Recommendation: the consumer marks a block as processed and purges it after about a minute, or treats slots idempotently.
2. Any disconnect loses blocks. The 10s Postgres outage left a 74-slot gap, because the provider reset the connection while the writers were not reading, and a new subscription does not replay. A silent stall can also lose up to 30s. Both writers can miss the same blocks, so the RPC recovery component is still needed.
3. parent_slot cannot find gaps on its own any more, because the parent row may already be deleted. Recovery will need to track the last slot seen separately.
4. If a signal arrives while Postgres is unavailable, the block in hand gets one more attempt and is then dropped with a `NOT saved` log line, so shutdown can't hang.

## 2026-10-31 17:36:22 -03

### Prompt

```
commit and push to main
```

### Response

Committed the block-saver work to main and pushed it to origin. The staged diff was checked first: .env is untracked and neither endpoint value appears in it.

## 2026-10-31 17:44:01 -03

### Prompt

```
Regarding the problems to be solved:

1. Deleting can bring a block back: We could leave a large window of blocks to be deleted but not yet deleted, but eventually that window might not be enough. To definitively solve this, we'll create another table, `last_indexed_block`, which records the slot of the last indexed block. Since we're not susceptible to reorgs, the slot is sufficient. We only insert a block into `blocks` if the slot is larger than the last indexed block.

2. Any disconnect loses blocks: We'll write another component for this later.

3. `parent_slot` can no longer find gaps on its own: We'll leave a window of a few blocks to detect gaps and calculate real-time latency for monitoring purposes. This is a problem that the next component we develop will solve; for now, we're fine.

4. Okay, I'm fine with that.
```

### Response

Problem 1 is fixed: block-saver now stores a block only if its slot is above `last_indexed_block`. Nothing is committed, and the stack is stopped with test data removed.

Changes:
- db/init/002_last_indexed_block.sql: a one-row table seeded with slot 0.
- crates/block-saver/src/main.rs: the insert now selects from that row `WHERE slot < $1 FOR SHARE`, and keeps ON CONFLICT DO NOTHING. A plain comparison would leave a small race where a writer checks just before the consumer commits and brings the deleted block back. FOR SHARE closes it: the consumer's UPDATE waits for in-flight inserts, and those inserts see a committed advance.
- The log line is now `already stored or indexed`.

Results with two writers on the live stream and a simulated consumer deleting within about 5 ms:

| Test | Before | After |
| --- | --- | --- |
| Times each slot was saved | Up to 4 (72 of 86 slots saved 4 times) | Exactly once (85 of 85) |

- Boundary: with the mark set ahead of the stream, the mark slot was rejected and `mark + 1` was the first stored, in two runs. Every slot above the mark was stored without gaps.
- One earlier run of the mark test stored its first block 13 slots above the mark. It did not recur, and that data was gone before I could check it. A burst of skipped slots is the likely cause, but that is not confirmed.
- `cargo fmt --check`, clippy with `-D warnings` and the 5 unit tests pass in Docker.

Rules the next component must follow:
1. In one short transaction: UPDATE last_indexed_block first, then DELETE the block. Writers wait while that transaction is open.
2. The last_indexed_block row must never be deleted. Without it, every block is rejected and only the `already stored or indexed` log line shows it.
3. Don't advance past a gap that hasn't been recovered. The guard will reject any recovered block at or below the mark, including one inserted through block-saver's query. Skipped slots are normal, so use parent_slot to tell them from lost blocks.

Any Postgres volume created before this change needs 002_last_indexed_block.sql applied by hand. None exists now.

No action on problems 2, 3 and 4, as agreed.

## 2026-10-31 18:01:58 -03

### Prompt

```
commit and push to main
```

### Response

Committed the `last_indexed_block` change to main and pushed it to origin. The staged diff was checked first: .env is untracked and neither endpoint value appears in it.
