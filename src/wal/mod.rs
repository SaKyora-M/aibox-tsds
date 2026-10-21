//! 预写日志（WAL）：记录格式、追加写与崩溃恢复重放。

pub mod record;
pub mod writer;

use std::path::Path;

use crate::error::Result;

pub use record::{PutRecord, RecordKind};
pub use writer::{list_segments, segment_name, Wal};

/// 重放结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recovery {
    /// 重放过的段数。
    pub segments: u64,
    /// 成功重放的记录数。
    pub records: u64,
    /// 最后一个段的有效字节数；`Some` 表示尾部有撕裂写，需截断到该长度。
    pub last_segment_valid_len: Option<u64>,
}

/// 按段号升序重放目录内全部 WAL，把每条写入记录交给 `apply`。
///
/// 最后一个段尾部的撕裂写 / 校验失败视为正常结束（掉电语义），其余损坏返回 `Corrupted`。
pub fn replay<F>(dir: &Path, mut apply: F) -> Result<Recovery>
where
    F: FnMut(PutRecord) -> Result<()>,
{
    let segments = list_segments(dir)?;
    let mut records = 0u64;
    let mut last_segment_valid_len = None;

    for (index, (_, path)) in segments.iter().enumerate() {
        let is_last = index + 1 == segments.len();
        let data = std::fs::read(path)?;
        let mut offset = 0usize;

        while offset < data.len() {
            match record::decode_record(&data, offset) {
                Ok((RecordKind::Put, payload, next)) => {
                    apply(PutRecord::decode_payload(payload)?)?;
                    records += 1;
                    offset = next;
                }
                Ok((RecordKind::Batch, payload, next)) => {
                    for (key, value) in record::decode_batch_payload(payload)? {
                        apply(PutRecord::new(key, value))?;
                        records += 1;
                    }
                    offset = next;
                }
                Err(_) if is_last => {
                    last_segment_valid_len = Some(offset as u64);
                    break;
                }
                Err(err) => return Err(err),
            }
        }
    }

    Ok(Recovery {
        segments: segments.len() as u64,
        records,
        last_segment_valid_len,
    })
}
