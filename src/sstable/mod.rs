//! SSTable：数据块 + 块索引 + 文件尾的有序磁盘文件。

pub mod block;
pub mod builder;
pub mod reader;

use crate::encoding::{InternalKey, KEY_HEADER_LEN};
use crate::error::{Result, TsdsError};

pub use builder::{SstableBuilder, SstableMeta};
pub use reader::{SstableIter, SstableReader};

/// 文件尾魔数。
pub(crate) const MAGIC: [u8; 4] = *b"TSDS";

/// 文件尾长度：index_offset(8) + index_len(4) + magic(4)。
pub(crate) const FOOTER_LEN: usize = 16;

/// 构造 SST 相关错误。
pub(crate) fn corrupt(detail: impl Into<String>) -> TsdsError {
    TsdsError::Corrupted {
        context: "sst",
        detail: detail.into(),
    }
}

/// 块索引项：每块的最后键与文件偏移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub(crate) last_key: InternalKey,
    pub(crate) offset: u64,
    pub(crate) len: u32,
}

pub(crate) fn encode_index(entries: &[IndexEntry]) -> Vec<u8> {
    let mut buf = Vec::new();
    for entry in entries {
        let key = entry.last_key.as_bytes();
        buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buf.extend_from_slice(key);
        buf.extend_from_slice(&entry.offset.to_le_bytes());
        buf.extend_from_slice(&entry.len.to_le_bytes());
    }
    buf
}

pub(crate) fn decode_index(bytes: &[u8]) -> Result<Vec<IndexEntry>> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if offset + 4 > bytes.len() {
            return Err(corrupt("index: truncated key_len"));
        }
        let key_len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        offset += 4;
        if key_len < KEY_HEADER_LEN || offset + key_len + 12 > bytes.len() {
            return Err(corrupt("index: invalid entry bounds"));
        }
        let last_key = InternalKey::from_bytes(bytes[offset..offset + key_len].to_vec())?;
        offset += key_len;
        let block_offset =
            u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes"));
        offset += 8;
        let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"));
        offset += 4;
        entries.push(IndexEntry {
            last_key,
            offset: block_offset,
            len,
        });
    }
    Ok(entries)
}
