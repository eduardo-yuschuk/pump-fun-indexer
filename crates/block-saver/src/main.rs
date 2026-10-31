use std::env;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::value::RawValue;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

type Error = Box<dyn std::error::Error + Send + Sync>;

const SUBSCRIBE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"blockSubscribe","params":["all",{"commitment":"finalized","encoding":"json","transactionDetails":"full","maxSupportedTransactionVersion":1,"showRewards":false}]}"#;

const INSERT: &str = "INSERT INTO blocks (slot, parent_slot, block_time, readed_time, data)
    SELECT $1, $2, $3, $4, $5::json
    FROM last_indexed_block
    WHERE slot < $1
    FOR SHARE
    ON CONFLICT (slot) DO NOTHING";

// Blocks arrive every ~400ms, so this much silence means the connection is dead.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const SAVE_RETRY_DELAY: Duration = Duration::from_secs(1);
const DB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(borrow)]
    params: Option<Params<'a>>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Params<'a> {
    #[serde(borrow)]
    result: Notification<'a>,
}

#[derive(Deserialize)]
struct Notification<'a> {
    #[serde(borrow)]
    value: NotificationValue<'a>,
}

#[derive(Deserialize)]
struct NotificationValue<'a> {
    slot: i64,
    #[serde(borrow)]
    block: Option<&'a RawValue>,
    #[serde(borrow)]
    err: Option<&'a RawValue>,
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

/// Returns the block carried by a message, or None for messages without one.
/// `data` is a slice of `text`, so the JSON is stored exactly as received.
fn parse_message(text: &str) -> Result<Option<Block<'_>>, Error> {
    let envelope: Envelope = serde_json::from_str(text)?;
    if let Some(error) = envelope.error {
        return Err(format!("provider error: {error}").into());
    }
    // Only the subscription acknowledgement has no params.
    let Some(params) = envelope.params else {
        return Ok(None);
    };
    let value = params.result.value;
    let Some(block) = value.block else {
        let err = value.err.map_or("null", RawValue::get);
        eprintln!(
            "slot {}: notification without block, err: {err}",
            value.slot
        );
        return Ok(None);
    };
    let header: BlockHeader = serde_json::from_str(block.get())?;
    Ok(Some(Block {
        slot: value.slot,
        parent_slot: header.parent_slot,
        block_time: header
            .block_time
            .and_then(|secs| DateTime::from_timestamp(secs, 0)),
        data: block.get(),
    }))
}

/// `wait` is how long the frame took to arrive once the previous block was done.
/// `lag_ms` is only accurate to a second, because `blockTime` has that resolution.
fn block_trace(
    block: &Block<'_>,
    status: &str,
    readed_time: DateTime<Utc>,
    wait: Duration,
    parse: Duration,
    save: Duration,
) -> String {
    let lag_ms = block.block_time.map_or("-".to_string(), |time| {
        (readed_time - time).num_milliseconds().to_string()
    });
    format!(
        "{} slot={} parent={} status={status} bytes={} lag_ms={lag_ms} wait_ms={:.1} parse_ms={:.1} save_ms={:.1} total_ms={:.1}",
        readed_time.to_rfc3339_opts(SecondsFormat::Millis, true),
        block.slot,
        block.parent_slot,
        block.data.len(),
        wait.as_secs_f64() * 1e3,
        parse.as_secs_f64() * 1e3,
        save.as_secs_f64() * 1e3,
        (parse + save).as_secs_f64() * 1e3,
    )
}

/// Retries until the block is persisted. Once shutdown is requested a failed
/// attempt is final, so a database outage can't keep the process from exiting.
/// Returns the outcome for the block trace.
async fn save_block(
    pool: &PgPool,
    block: &Block<'_>,
    readed_time: DateTime<Utc>,
    shutdown: &watch::Receiver<bool>,
) -> &'static str {
    loop {
        let result = sqlx::query(INSERT)
            .bind(block.slot)
            .bind(block.parent_slot)
            .bind(block.block_time)
            .bind(readed_time)
            .bind(block.data)
            .execute(pool)
            .await;
        match result {
            // Already stored or indexed.
            Ok(done) if done.rows_affected() == 0 => return "skipped",
            Ok(_) => return "saved",
            Err(err) if *shutdown.borrow() => {
                eprintln!("slot {}: NOT saved, shutting down: {err}", block.slot);
                return "failed";
            }
            Err(err) => {
                eprintln!("slot {}: save failed, retrying: {err}", block.slot);
                sleep(SAVE_RETRY_DELAY).await;
            }
        }
    }
}

/// Runs one connection. Returns Ok on shutdown and Err when the connection is lost.
async fn read_blocks(
    endpoint: &str,
    pool: &PgPool,
    shutdown: &mut watch::Receiver<bool>,
    backoff: &mut Duration,
) -> Result<(), Error> {
    // Blocks can exceed the default frame limit, which would drop the connection.
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let connect = timeout(
        READ_TIMEOUT,
        connect_async_with_config(endpoint, Some(config), false),
    );
    let (mut ws, _) = tokio::select! {
        _ = shutdown.wait_for(|stop| *stop) => return Ok(()),
        result = connect => result.map_err(|_| "connect timed out")??,
    };
    ws.send(Message::text(SUBSCRIBE)).await?;
    println!("connected, subscribing");

    // Only block notifications move the deadline: pings prove nothing about the stream.
    let mut ready_at = Instant::now();
    loop {
        // The save below runs outside this select, so a signal can't interrupt it.
        let message = tokio::select! {
            biased;
            _ = shutdown.wait_for(|stop| *stop) => return Ok(()),
            message = timeout_at(ready_at + READ_TIMEOUT, ws.next()) => message,
        };
        let message = message
            .map_err(|_| "no block received within the read timeout")?
            .ok_or("stream closed")??;
        let received_at = Instant::now();
        let readed_time = Utc::now();
        let Message::Text(text) = message else {
            continue;
        };
        if let Some(block) = parse_message(text.as_str())? {
            let parsed_at = Instant::now();
            let status = save_block(pool, &block, readed_time, shutdown).await;
            let saved_at = Instant::now();
            println!(
                "{}",
                block_trace(
                    &block,
                    status,
                    readed_time,
                    received_at - ready_at,
                    parsed_at - received_at,
                    saved_at - parsed_at,
                )
            );
            *backoff = MIN_BACKOFF;
            ready_at = saved_at;
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let endpoint = env::var("WSS_ENDPOINT")?;
    let database_url = env::var("DATABASE_URL")?;
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

    let mut backoff = MIN_BACKOFF;
    while let Err(err) = read_blocks(&endpoint, &pool, &mut shutdown, &mut backoff).await {
        eprintln!("connection lost: {err}; reconnecting in {backoff:?}");
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => break,
            _ = sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }

    pool.close().await;
    println!("stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_block_keeping_json_verbatim() {
        let data = r#"{"blockTime": 1700000000,"zeta":1,"parentSlot":99,"transactions":[]}"#;
        let text = format!(
            r#"{{"jsonrpc":"2.0","method":"blockNotification","params":{{"result":{{"context":{{"slot":100}},"value":{{"slot":100,"err":null,"block":{data}}}}},"subscription":7}}}}"#
        );
        assert_eq!(
            parse_message(&text).unwrap(),
            Some(Block {
                slot: 100,
                parent_slot: 99,
                block_time: DateTime::from_timestamp(1_700_000_000, 0),
                data,
            })
        );
    }

    #[test]
    fn traces_block() {
        let readed_time = DateTime::from_timestamp_millis(1_700_000_001_500).unwrap();
        let block = Block {
            slot: 100,
            parent_slot: 99,
            block_time: DateTime::from_timestamp(1_700_000_000, 0),
            data: "{}",
        };
        assert_eq!(
            block_trace(
                &block,
                "saved",
                readed_time,
                Duration::from_millis(130),
                Duration::from_micros(2500),
                Duration::from_millis(14),
            ),
            "2023-11-14T22:13:21.500Z slot=100 parent=99 status=saved bytes=2 lag_ms=1500 wait_ms=130.0 parse_ms=2.5 save_ms=14.0 total_ms=16.5"
        );
    }

    #[test]
    fn accepts_null_block_time() {
        let text = r#"{"params":{"result":{"value":{"slot":5,"err":null,"block":{"parentSlot":4,"blockTime":null}}}}}"#;
        assert_eq!(parse_message(text).unwrap().unwrap().block_time, None);
    }

    #[test]
    fn ignores_subscription_acknowledgement() {
        let text = r#"{"jsonrpc":"2.0","result":7,"id":1}"#;
        assert_eq!(parse_message(text).unwrap(), None);
    }

    #[test]
    fn ignores_notification_without_block() {
        let text =
            r#"{"params":{"result":{"value":{"slot":5,"err":"BlockStoreError","block":null}}}}"#;
        assert_eq!(parse_message(text).unwrap(), None);
    }

    #[test]
    fn provider_error_is_an_error() {
        let text =
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
        assert!(parse_message(text).is_err());
    }
}
