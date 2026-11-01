use std::env;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::value::RawValue;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::time::sleep;

type Error = Box<dyn std::error::Error + Send + Sync>;

// GREATEST ignores NULL. The indexer deletes verified blocks, so the mark is
// what remains once they are all consumed.
const LOAD_FRONTIER: &str = "SELECT GREATEST(slot, (SELECT max(slot) FROM blocks WHERE verified))
    FROM last_indexed_block";

const NEXT_BLOCK: &str = "SELECT slot, parent_slot FROM blocks
    WHERE slot > $1
    ORDER BY slot
    LIMIT 1";

const RECOVER: &str = "INSERT INTO blocks (slot, parent_slot, block_time, readed_time, data)
    VALUES ($1, $2, $3, $4, $5::json)
    ON CONFLICT (slot) DO NOTHING";

// One statement, so the frontier block is never verified without the skipped
// slots below it. It also keeps max(verified slot) on a real block.
const VERIFY: &str = "WITH skipped AS (
        INSERT INTO blocks (slot, skipped, verified)
        SELECT generate_series($1::bigint, $2::bigint - 1), true, true
        ON CONFLICT (slot) DO NOTHING
    )
    UPDATE blocks SET verified = true WHERE slot = $2";

// Slots appear every ~400ms.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const RPC_TIMEOUT: Duration = Duration::from_secs(20);
const DB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct RpcResponse<'a> {
    #[serde(borrow)]
    result: Option<&'a RawValue>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockHeader {
    parent_slot: i64,
    block_time: Option<i64>,
}

#[derive(Debug, PartialEq)]
struct Block<'a> {
    slot: i64,
    parent_slot: i64,
    block_time: Option<DateTime<Utc>>,
    data: &'a str,
}

#[derive(Debug, PartialEq)]
enum Step {
    Wait,
    Recover(i64),
    Verify { first_skipped: i64, slot: i64 },
}

/// Decides what to do with the lowest stored slot above the frontier, the last
/// verified slot. Frontier 0 means nothing was ever verified, and the first
/// stored block becomes the start of the chain.
fn next_step(frontier: i64, next: Option<(i64, i64)>) -> Result<Step, Error> {
    let Some((slot, parent_slot)) = next else {
        return Ok(Step::Wait);
    };
    if frontier == 0 || parent_slot == frontier {
        let first_skipped = if frontier == 0 { slot } else { frontier + 1 };
        return Ok(Step::Verify {
            first_skipped,
            slot,
        });
    }
    if parent_slot > frontier {
        return Ok(Step::Recover(parent_slot));
    }
    Err(format!("slot {slot}: parent {parent_slot} is below verified slot {frontier}").into())
}

fn get_block_request(slot: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"getBlock","params":[{slot},{{"commitment":"finalized","encoding":"json","transactionDetails":"full","maxSupportedTransactionVersion":1,"rewards":false}}]}}"#
    )
}

/// `data` is a slice of `text`, so the JSON is stored exactly as received. Slots
/// requested here are parents of a stored block, so "no block" is an error too.
fn parse_block(slot: i64, text: &str) -> Result<Block<'_>, Error> {
    let response: RpcResponse = serde_json::from_str(text)?;
    if let Some(error) = response.error {
        return Err(format!("provider error: {error}").into());
    }
    let block = response.result.ok_or("provider returned no block")?;
    let header: BlockHeader = serde_json::from_str(block.get())?;
    Ok(Block {
        slot,
        parent_slot: header.parent_slot,
        block_time: header
            .block_time
            .and_then(|secs| DateTime::from_timestamp(secs, 0)),
        data: block.get(),
    })
}

/// Fetches and stores one slot. Errors drop the URL because it holds the API key.
async fn recover_block(
    pool: &PgPool,
    client: &Client,
    endpoint: &str,
    slot: i64,
) -> Result<(), Error> {
    let started_at = Instant::now();
    let text = client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(get_block_request(slot))
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(reqwest::Error::without_url)?
        .text()
        .await
        .map_err(reqwest::Error::without_url)?;
    let rpc = started_at.elapsed();
    let block = parse_block(slot, &text)?;
    let readed_time = Utc::now();
    sqlx::query(RECOVER)
        .bind(block.slot)
        .bind(block.parent_slot)
        .bind(block.block_time)
        .bind(readed_time)
        .bind(block.data)
        .execute(pool)
        .await?;
    println!(
        "{} slot={} parent={} status=recovered bytes={} rpc_ms={:.1}",
        readed_time.to_rfc3339_opts(SecondsFormat::Millis, true),
        block.slot,
        block.parent_slot,
        block.data.len(),
        rpc.as_secs_f64() * 1e3,
    );
    Ok(())
}

/// Takes one step along the chain. Returns false when there is nothing to do yet.
async fn advance(
    pool: &PgPool,
    client: &Client,
    endpoint: &str,
    frontier: &mut i64,
) -> Result<bool, Error> {
    let next: Option<(i64, i64)> = sqlx::query_as(NEXT_BLOCK)
        .bind(*frontier)
        .fetch_optional(pool)
        .await?;
    match next_step(*frontier, next)? {
        Step::Wait => Ok(false),
        // The recovered block becomes the next one read, so this walks parents
        // down to the frontier. Progress survives a restart.
        Step::Recover(slot) => {
            recover_block(pool, client, endpoint, slot).await?;
            Ok(true)
        }
        Step::Verify {
            first_skipped,
            slot,
        } => {
            sqlx::query(VERIFY)
                .bind(first_skipped)
                .bind(slot)
                .execute(pool)
                .await?;
            println!(
                "{} slot={slot} status=verified skipped={}",
                Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                slot - first_skipped,
            );
            *frontier = slot;
            Ok(true)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let endpoint = env::var("HTTPS_ENDPOINT")?;
    let database_url = env::var("DATABASE_URL")?;
    let client = Client::builder().timeout(RPC_TIMEOUT).build()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(DB_ACQUIRE_TIMEOUT)
        .connect_lazy(&database_url)?;

    let (shutdown_tx, mut shutdown) = watch::channel(false);
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        println!("shutdown requested");
        let _ = shutdown_tx.send(true);
    });

    let mut frontier: i64 = sqlx::query_scalar(LOAD_FRONTIER).fetch_one(&pool).await?;
    println!("starting after slot {frontier}");

    // A step always runs to completion: only the waits below watch for shutdown.
    while !*shutdown.borrow() {
        let delay = match advance(&pool, &client, &endpoint, &mut frontier).await {
            Ok(true) => continue,
            Ok(false) => POLL_INTERVAL,
            Err(err) => {
                eprintln!("verification stalled after slot {frontier}, retrying: {err}");
                RETRY_DELAY
            }
        };
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = sleep(delay) => {}
        }
    }

    pool.close().await;
    println!("stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_without_a_block_above_the_frontier() {
        assert_eq!(next_step(100, None).unwrap(), Step::Wait);
    }

    #[test]
    fn verifies_consecutive_block() {
        assert_eq!(
            next_step(100, Some((101, 100))).unwrap(),
            Step::Verify {
                first_skipped: 101,
                slot: 101
            }
        );
    }

    #[test]
    fn slots_between_parent_and_child_are_skipped() {
        assert_eq!(
            next_step(100, Some((104, 100))).unwrap(),
            Step::Verify {
                first_skipped: 101,
                slot: 104
            }
        );
    }

    #[test]
    fn recovers_the_missing_parent() {
        assert_eq!(
            next_step(100, Some((104, 103))).unwrap(),
            Step::Recover(103)
        );
    }

    #[test]
    fn first_block_starts_the_chain() {
        assert_eq!(
            next_step(0, Some((500, 499))).unwrap(),
            Step::Verify {
                first_skipped: 500,
                slot: 500
            }
        );
    }

    #[test]
    fn parent_below_the_frontier_is_an_error() {
        assert!(next_step(100, Some((104, 99))).is_err());
    }

    #[test]
    fn parses_block_keeping_json_verbatim() {
        let data = r#"{"blockTime": 1700000000,"zeta":1,"parentSlot":99,"transactions":[]}"#;
        let text = format!(r#"{{"jsonrpc":"2.0","result":{data},"id":1}}"#);
        assert_eq!(
            parse_block(100, &text).unwrap(),
            Block {
                slot: 100,
                parent_slot: 99,
                block_time: DateTime::from_timestamp(1_700_000_000, 0),
                data,
            }
        );
    }

    #[test]
    fn provider_error_is_an_error() {
        let text =
            r#"{"jsonrpc":"2.0","error":{"code":-32004,"message":"Block not available"},"id":1}"#;
        assert!(parse_block(100, text).is_err());
    }

    #[test]
    fn null_result_is_an_error() {
        assert!(parse_block(100, r#"{"jsonrpc":"2.0","result":null,"id":1}"#).is_err());
    }
}
