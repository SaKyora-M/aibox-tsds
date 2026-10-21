//! 内部键编码：`channel(4B BE) ‖ timestamp(8B BE, 符号位翻转) ‖ user_key`。

use crate::error::{Result, TsdsError};
use std::fmt;

/// 内部键头部长度：4 字节 channel + 8 字节 timestamp。
pub const KEY_HEADER_LEN: usize = 4 + 8;

/// i64 时间戳的排序变换掩码（翻转符号位，使负值也按字节序正确排列）。
const TS_SORT_MASK: u64 = 1 << 63;

/// 把 `(channel, timestamp_ms, user_key)` 追加编码到 `buf` 末尾。
pub(crate) fn encode_key_into(buf: &mut Vec<u8>, channel: u32, timestamp_ms: i64, user_key: &[u8]) {
    buf.extend_from_slice(&channel.to_be_bytes());
    buf.extend_from_slice(&((timestamp_ms as u64) ^ TS_SORT_MASK).to_be_bytes());
    buf.extend_from_slice(user_key);
}

/// 已编码的内部键。
///
/// 只能经 [`InternalKey::new`] 或 [`InternalKey::from_bytes`] 构造，以保证
/// "至少 12 字节定长头部"这一不变量。`Ord` 按字节字典序实现，等价于
/// `(channel, timestamp, user_key)` 的字典序。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InternalKey(Vec<u8>);

impl InternalKey {
    /// 由三元组构造内部键。
    pub fn new(channel: u32, timestamp_ms: i64, user_key: &[u8]) -> Self {
        let mut buf = Vec::with_capacity(KEY_HEADER_LEN + user_key.len());
        encode_key_into(&mut buf, channel, timestamp_ms, user_key);
        InternalKey(buf)
    }

    /// 由已有字节构造内部键；长度不足 [`KEY_HEADER_LEN`] 时返回 `Corrupted`。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < KEY_HEADER_LEN {
            return Err(TsdsError::Corrupted {
                context: "encoding.internal_key",
                detail: format!("key too short: {} bytes < {KEY_HEADER_LEN}", bytes.len()),
            });
        }
        Ok(InternalKey(bytes))
    }

    /// 原始字节。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 通道号。
    pub fn channel(&self) -> u32 {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.0[..4]);
        u32::from_be_bytes(buf)
    }

    /// 时间戳（毫秒，已还原符号位）。
    pub fn timestamp_ms(&self) -> i64 {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.0[4..KEY_HEADER_LEN]);
        (u64::from_be_bytes(buf) ^ TS_SORT_MASK) as i64
    }

    /// 用户键部分（可能为空）。
    pub fn user_key(&self) -> &[u8] {
        &self.0[KEY_HEADER_LEN..]
    }

    /// 一次性取出三元组。
    pub fn parts(&self) -> (u32, i64, &[u8]) {
        (self.channel(), self.timestamp_ms(), self.user_key())
    }
}

impl fmt::Debug for InternalKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (channel, ts, key) = self.parts();
        f.debug_struct("InternalKey")
            .field("channel", &channel)
            .field("ts", &ts)
            .field("user_key", &key)
            .finish()
    }
}

/// 计算时间范围 `[start_ms, end_ms)` 的扫描键区间 `[lo, hi)`。
///
/// `hi` 取 `channel ‖ ts(end_ms) ‖ ""`，是"ts == end_ms 的所有键"中最小的一个，
/// 作为排他上界恰好把 ts == end_ms 的记录全部挡在区间外，且不会溢出。
pub fn scan_bounds(channel: u32, start_ms: i64, end_ms: i64) -> Result<(InternalKey, InternalKey)> {
    if start_ms > end_ms {
        return Err(TsdsError::InvalidArgument("query range: start_ms > end_ms"));
    }
    Ok((
        InternalKey::new(channel, start_ms, &[]),
        InternalKey::new(channel, end_ms, &[]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn ts_order_matches_numeric_order() {
        let samples = [i64::MIN, -1, 0, 1, i64::MAX];
        for &a in &samples {
            for &b in &samples {
                let ka = InternalKey::new(7, a, b"");
                let kb = InternalKey::new(7, b, b"");
                assert_eq!(a.cmp(&b), ka.cmp(&kb), "ts {a} vs {b}");
            }
        }
    }

    #[test]
    fn channel_is_primary() {
        let k1 = InternalKey::new(1, i64::MAX, b"");
        let k2 = InternalKey::new(2, i64::MIN, b"");
        assert!(k1 < k2);
    }

    #[test]
    fn user_key_is_tiebreak() {
        let empty = InternalKey::new(1, 100, b"");
        let a = InternalKey::new(1, 100, b"a");
        let b = InternalKey::new(1, 100, b"b");
        assert!(empty < a);
        assert!(a < b);
    }

    #[test]
    fn roundtrip_parts() {
        let samples: [(u32, i64, &[u8]); 3] = [
            (0, 0, b""),
            (3, 1_700_000_000_000, b"alarm"),
            (u32::MAX, i64::MIN, b"x"),
        ];
        for (ch, ts, key) in samples {
            let k = InternalKey::new(ch, ts, key);
            assert_eq!(k.parts(), (ch, ts, key));
            let k2 = InternalKey::from_bytes(k.as_bytes().to_vec()).unwrap();
            assert_eq!(k2, k);
        }
    }

    #[test]
    fn from_bytes_rejects_short() {
        assert!(InternalKey::from_bytes(Vec::new()).is_err());
        assert!(InternalKey::from_bytes(vec![0u8; KEY_HEADER_LEN - 1]).is_err());
        assert!(InternalKey::from_bytes(vec![0u8; KEY_HEADER_LEN]).is_ok());
    }

    #[test]
    fn scan_bounds_left_closed_right_open() {
        let records: [(i64, &[u8]); 7] = [
            (99, b""),
            (100, b""),
            (100, b"a"),
            (150, b""),
            (200, b""),
            (200, b"a"),
            (201, b""),
        ];
        let mut map = BTreeMap::new();
        for (ts, key) in records {
            map.insert(InternalKey::new(5, ts, key), ());
        }

        let (lo, hi) = scan_bounds(5, 100, 200).unwrap();
        let got: Vec<(i64, Vec<u8>)> = map
            .range(lo..hi)
            .map(|(k, _)| {
                let (_, ts, key) = k.parts();
                (ts, key.to_vec())
            })
            .collect();

        assert_eq!(
            got,
            vec![
                (100, b"".to_vec()),
                (100, b"a".to_vec()),
                (150, b"".to_vec())
            ]
        );
    }

    #[test]
    fn empty_range_is_empty() {
        let mut map = BTreeMap::new();
        map.insert(InternalKey::new(5, 100, b""), ());
        let (lo, hi) = scan_bounds(5, 100, 100).unwrap();
        assert_eq!(map.range(lo..hi).count(), 0);
    }
}
