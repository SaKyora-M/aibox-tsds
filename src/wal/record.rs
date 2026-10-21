//! WAL 物理记录格式与编解码。
//!
//! 单条记录：`crc32c(4B LE 掩码) ‖ len(4B LE) ‖ kind(1B) ‖ payload(len B)`。
//! CRC 覆盖 `kind ‖ payload`。

use crate::checksum;
use crate::encoding::{InternalKey, KEY_HEADER_LEN};
use crate::error::{Result, TsdsError};

/// 记录头长度：crc(4) + len(4) + kind(1)。
pub const HEADER_LEN: usize = 9;

/// 单条记录 payload 上限，用于识别损坏的长度字段。
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

/// 记录类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordKind {
    /// 单条写入。
    Put = 1,
    /// 批量写入（一次提交多条）。
    Batch = 2,
}

impl RecordKind {
    /// 由字节值还原记录类型。
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(RecordKind::Put),
            2 => Ok(RecordKind::Batch),
            other => Err(corrupted(format!("unknown record kind {other}"))),
        }
    }
}

fn corrupted(detail: String) -> TsdsError {
    TsdsError::Corrupted {
        context: "wal.record",
        detail,
    }
}

/// 一条写入记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutRecord {
    /// 内部键。
    pub key: InternalKey,
    /// 值字节。
    pub value: Vec<u8>,
}

impl PutRecord {
    /// 构造写入记录。
    pub fn new(key: InternalKey, value: Vec<u8>) -> Self {
        Self { key, value }
    }

    /// 编码 payload：`key_len(4B LE) ‖ key ‖ value`。
    pub fn encode_payload(&self) -> Vec<u8> {
        let key_bytes = self.key.as_bytes();
        let mut buf = Vec::with_capacity(4 + key_bytes.len() + self.value.len());
        buf.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(key_bytes);
        buf.extend_from_slice(&self.value);
        buf
    }

    /// 解析 payload。
    pub fn decode_payload(payload: &[u8]) -> Result<Self> {
        if payload.len() < 4 {
            return Err(corrupted("put payload shorter than key_len".into()));
        }
        let key_len = u32::from_le_bytes(payload[..4].try_into().expect("4 bytes")) as usize;
        if key_len < KEY_HEADER_LEN || 4 + key_len > payload.len() {
            return Err(corrupted(format!("invalid key_len {key_len}")));
        }
        let key = InternalKey::from_bytes(payload[4..4 + key_len].to_vec())?;
        let value = payload[4 + key_len..].to_vec();
        Ok(Self { key, value })
    }
}

/// 编码批量记录 payload：
/// `count(4B LE) ‖ [key_len(4B LE) ‖ key ‖ value_len(4B LE) ‖ value] * count`
pub fn encode_batch_payload(items: &[(InternalKey, Vec<u8>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for (key, value) in items {
        let key_bytes = key.as_bytes();
        buf.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(key_bytes);
        buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
        buf.extend_from_slice(value);
    }
    buf
}

/// 解析批量记录 payload。
pub fn decode_batch_payload(payload: &[u8]) -> Result<Vec<(InternalKey, Vec<u8>)>> {
    if payload.len() < 4 {
        return Err(corrupted("batch payload shorter than count".into()));
    }
    let count = u32::from_le_bytes(payload[..4].try_into().expect("4 bytes")) as usize;
    let mut items = Vec::with_capacity(count.min(1024));
    let mut offset = 4usize;

    for _ in 0..count {
        if offset + 4 > payload.len() {
            return Err(corrupted("batch: truncated key_len".into()));
        }
        let key_len =
            u32::from_le_bytes(payload[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        offset += 4;
        if key_len < KEY_HEADER_LEN || offset + key_len > payload.len() {
            return Err(corrupted("batch: invalid key_len".into()));
        }
        let key = InternalKey::from_bytes(payload[offset..offset + key_len].to_vec())?;
        offset += key_len;

        if offset + 4 > payload.len() {
            return Err(corrupted("batch: truncated value_len".into()));
        }
        let value_len =
            u32::from_le_bytes(payload[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        offset += 4;
        if offset + value_len > payload.len() {
            return Err(corrupted("batch: truncated value".into()));
        }
        items.push((key, payload[offset..offset + value_len].to_vec()));
        offset += value_len;
    }

    if offset != payload.len() {
        return Err(corrupted("batch: trailing bytes".into()));
    }
    Ok(items)
}

/// 把 `kind` 与 `payload` 编码为一条完整物理记录。
pub fn encode_record(kind: RecordKind, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(TsdsError::InvalidArgument(
            "wal payload exceeds MAX_PAYLOAD_LEN",
        ));
    }
    let kind_byte = kind as u8;
    let kind_slice = [kind_byte];
    let crc = checksum::mask(checksum::crc32c_many(&[&kind_slice[..], payload]));

    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.push(kind_byte);
    buf.extend_from_slice(payload);
    Ok(buf)
}

/// 从 `data[offset..]` 解析一条记录，返回 `(kind, payload, 下一条记录偏移)`。
pub fn decode_record(data: &[u8], offset: usize) -> Result<(RecordKind, &[u8], usize)> {
    let header_end = offset + HEADER_LEN;
    if data.len() < header_end {
        return Err(corrupted("truncated record header".into()));
    }
    let crc = u32::from_le_bytes(data[offset..offset + 4].try_into().expect("4 bytes"));
    let len =
        u32::from_le_bytes(data[offset + 4..offset + 8].try_into().expect("4 bytes")) as usize;
    let kind_byte = data[offset + 8];
    let kind = RecordKind::from_u8(kind_byte)?;
    if len > MAX_PAYLOAD_LEN {
        return Err(corrupted(format!("payload len {len} exceeds limit")));
    }
    let payload_end = header_end + len;
    if data.len() < payload_end {
        return Err(corrupted("truncated record payload".into()));
    }
    let payload = &data[header_end..payload_end];
    let kind_slice = [kind_byte];
    let actual = checksum::mask(checksum::crc32c_many(&[&kind_slice[..], payload]));
    if actual != crc {
        return Err(corrupted(format!("crc mismatch at offset {offset}")));
    }
    Ok((kind, payload, payload_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PutRecord {
        PutRecord::new(
            InternalKey::new(3, 1_700_000_000_000, b"alarm"),
            b"person".to_vec(),
        )
    }

    #[test]
    fn record_roundtrip() {
        let record = sample();
        let bytes = encode_record(RecordKind::Put, &record.encode_payload()).unwrap();
        let (kind, payload, next) = decode_record(&bytes, 0).unwrap();
        assert_eq!(kind, RecordKind::Put);
        assert_eq!(next, bytes.len());
        assert_eq!(PutRecord::decode_payload(payload).unwrap(), record);
    }

    #[test]
    fn crc_mismatch_rejected() {
        let record = sample();
        let mut bytes = encode_record(RecordKind::Put, &record.encode_payload()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert!(decode_record(&bytes, 0).is_err());
    }

    #[test]
    fn truncated_payload_rejected() {
        let record = sample();
        let bytes = encode_record(RecordKind::Put, &record.encode_payload()).unwrap();
        assert!(decode_record(&bytes[..bytes.len() - 1], 0).is_err());
    }

    #[test]
    fn put_payload_roundtrip_empty_value() {
        let record = PutRecord::new(InternalKey::new(0, 0, b""), Vec::new());
        assert_eq!(
            PutRecord::decode_payload(&record.encode_payload()).unwrap(),
            record
        );
    }

    #[test]
    fn batch_payload_roundtrip() {
        let items = vec![
            (InternalKey::new(1, 10, b"a"), b"v1".to_vec()),
            (InternalKey::new(1, 11, b""), Vec::new()),
            (InternalKey::new(2, 12, b"ccc"), b"v3".to_vec()),
        ];
        let payload = encode_batch_payload(&items);
        assert_eq!(decode_batch_payload(&payload).unwrap(), items);
    }

    #[test]
    fn batch_payload_rejects_trailing_bytes() {
        let items = vec![(InternalKey::new(1, 10, b""), b"v".to_vec())];
        let mut payload = encode_batch_payload(&items);
        payload.push(0xEE);
        assert!(decode_batch_payload(&payload).is_err());
    }
}
