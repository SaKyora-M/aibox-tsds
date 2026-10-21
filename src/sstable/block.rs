//! SST 数据块编解码：`key_len(4B LE) ‖ key ‖ value_len(4B LE) ‖ value`。

use crate::encoding::{InternalKey, KEY_HEADER_LEN};
use crate::error::Result;

use super::corrupt;

/// 把一条条目追加到块缓冲。
pub(crate) fn encode_entry(buf: &mut Vec<u8>, key: &InternalKey, value: &[u8]) {
    buf.extend_from_slice(&(key.as_bytes().len() as u32).to_le_bytes());
    buf.extend_from_slice(key.as_bytes());
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value);
}

/// 解析整块为条目列表。
pub(crate) fn decode_block(bytes: &[u8]) -> Result<Vec<(InternalKey, Vec<u8>)>> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if offset + 4 > bytes.len() {
            return Err(corrupt("block: truncated key_len"));
        }
        let key_len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        offset += 4;
        if key_len < KEY_HEADER_LEN || offset + key_len > bytes.len() {
            return Err(corrupt("block: invalid key_len"));
        }
        let key = InternalKey::from_bytes(bytes[offset..offset + key_len].to_vec())?;
        offset += key_len;

        if offset + 4 > bytes.len() {
            return Err(corrupt("block: truncated value_len"));
        }
        let value_len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        offset += 4;
        if offset + value_len > bytes.len() {
            return Err(corrupt("block: truncated value"));
        }
        entries.push((key, bytes[offset..offset + value_len].to_vec()));
        offset += value_len;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_roundtrip() {
        let mut buf = Vec::new();
        encode_entry(&mut buf, &InternalKey::new(1, 5, b"a"), b"v1");
        encode_entry(&mut buf, &InternalKey::new(1, 6, b""), b"");

        let entries = decode_block(&buf).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, InternalKey::new(1, 5, b"a"));
        assert_eq!(entries[0].1, b"v1".to_vec());
        assert_eq!(entries[1].1, Vec::<u8>::new());
    }

    #[test]
    fn decode_rejects_truncated() {
        let mut buf = Vec::new();
        encode_entry(&mut buf, &InternalKey::new(1, 5, b""), b"value");
        assert!(decode_block(&buf[..buf.len() - 1]).is_err());
    }
}
