use cellora_common::events::{BlockMinedEvent, CellCreatedEvent, BLOCKS_CHANNEL, CELLS_CHANNEL};
use cellora_db::models::{BlockRow, CellRow, HashType};
use redis::{AsyncCommands, RedisResult};
use redis::aio::ConnectionManager;
use tracing::{error, info, warn};

pub async fn publish_block_and_cells(
    redis: &ConnectionManager,
    block: &BlockRow,
    cells: &[CellRow],
) {
    let mut conn = redis.clone();

    // 1. Publish Block
    let block_event = BlockMinedEvent {
        number: block.number,
        hash: hex::encode(&block.hash),
        parent_hash: hex::encode(&block.parent_hash),
        timestamp: block.timestamp_ms as u64,
    };

    let payload = match serde_json::to_string(&block_event) {
        Ok(p) => p,
        Err(err) => {
            error!(error = %err, "serialise block event failed");
            return;
        }
    };

    let publish: RedisResult<i64> = conn.publish(BLOCKS_CHANNEL, payload).await;
    match publish {
        Ok(receivers) => {
            info!(receivers, channel = BLOCKS_CHANNEL, "published block event");
        }
        Err(err) => {
            warn!(error = %err, "publish block event failed");
        }
    }

    // 2. Publish Cells
    for cell in cells {
        let cell_event = CellCreatedEvent {
            tx_hash: hex::encode(&cell.tx_hash),
            output_index: cell.output_index,
            block_number: cell.block_number,
            lock_code_hash: hex::encode(&cell.lock_code_hash),
            lock_hash_type: cell.lock_hash_type.as_i16(),
            lock_args: hex::encode(&cell.lock_args),
            lock_hash: hex::encode(&cell.lock_hash),
            type_code_hash: cell.type_code_hash.as_ref().map(hex::encode),
            type_hash_type: cell.type_hash_type.map(|t: HashType| t.as_i16()),
            type_args: cell.type_args.as_ref().map(hex::encode),
            type_hash: cell.type_hash.as_ref().map(hex::encode),
            capacity_shannons: cell.capacity_shannons,
        };

        let payload = match serde_json::to_string(&cell_event) {
            Ok(p) => p,
            Err(err) => {
                error!(error = %err, "serialise cell event failed");
                continue;
            }
        };

        let publish: RedisResult<i64> = conn.publish(CELLS_CHANNEL, payload).await;
        if let Err(err) = publish {
            warn!(error = %err, "publish cell event failed");
        }
    }
}
