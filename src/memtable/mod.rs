//! 有序内存表（MemTable）：`InternalKey -> value`，按字节序排列。

use std::collections::BTreeMap;

use crate::encoding::InternalKey;
use crate::error::Result;

/// 每条记录除键值字节外的估算开销（BTreeMap 节点 + 元数据）。
const ENTRY_OVERHEAD_BYTES: usize = 48;

fn entry_bytes(key_len: usize, value_len: usize) -> usize {
    ENTRY_OVERHEAD_BYTES + key_len + value_len
}

/// 内存中的有序表。
#[derive(Debug, Default)]
pub struct MemTable {
    map: BTreeMap<InternalKey, Vec<u8>>,
    approx_bytes: usize,
}

impl MemTable {
    /// 新建空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 估算占用的字节数（键 + 值 + 每条固定开销）。
    pub fn approx_bytes(&self) -> usize {
        self.approx_bytes
    }

    /// 记录条数。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 写入或覆盖；返回被覆盖旧值的字节数。
    pub fn insert(&mut self, key: InternalKey, value: Vec<u8>) -> Option<usize> {
        let key_bytes = key.as_bytes().len();
        let new_value_bytes = value.len();
        let old = self.map.insert(key, value);
        match &old {
            None => self.approx_bytes += entry_bytes(key_bytes, new_value_bytes),
            Some(old_value) => {
                self.approx_bytes = self.approx_bytes - old_value.len() + new_value_bytes;
            }
        }
        old.map(|v| v.len())
    }

    /// 精确读取单条记录。
    pub fn get(&self, key: &InternalKey) -> Option<&[u8]> {
        self.map.get(key).map(Vec::as_slice)
    }

    /// 按通道与时间范围 `[start_ms, end_ms)` 顺序遍历。
    pub fn range(
        &self,
        channel: u32,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<impl Iterator<Item = (&InternalKey, &[u8])> + '_> {
        let (lo, hi) = crate::encoding::scan_bounds(channel, start_ms, end_ms)?;
        Ok(self.map.range(lo..hi).map(|(k, v)| (k, v.as_slice())))
    }

    /// 按序遍历全部记录。
    pub fn iter(&self) -> impl Iterator<Item = (&InternalKey, &[u8])> {
        self.map.iter().map(|(k, v)| (k, v.as_slice()))
    }

    /// 按内部键区间 `[lo, hi)` 遍历（显式生命周期，便于装箱为 `Cursor`）。
    pub fn iter_range<'a>(
        &'a self,
        lo: InternalKey,
        hi: InternalKey,
    ) -> impl Iterator<Item = (&'a InternalKey, &'a [u8])> + 'a {
        self.map.range(lo..hi).map(|(k, v)| (k, v.as_slice()))
    }

    /// 仅保留满足条件的记录，并重算内存占用。
    pub fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(&InternalKey) -> bool,
    {
        self.map.retain(|key, _| keep(key));
        self.approx_bytes = self
            .map
            .iter()
            .map(|(key, value)| entry_bytes(key.as_bytes().len(), value.len()))
            .sum();
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.map.clear();
        self.approx_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get() {
        let mut m = MemTable::new();
        assert!(m.is_empty());
        assert_eq!(m.insert(InternalKey::new(1, 10, b"a"), b"v".to_vec()), None);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&InternalKey::new(1, 10, b"a")), Some(&b"v"[..]));
        assert_eq!(m.get(&InternalKey::new(1, 10, b"b")), None);
    }

    #[test]
    fn overwrite_keeps_single_entry_and_adjusts_bytes() {
        let mut m = MemTable::new();
        m.insert(InternalKey::new(1, 10, b"a"), vec![0u8; 10]);
        let after_first = m.approx_bytes();
        m.insert(InternalKey::new(1, 10, b"a"), vec![0u8; 30]);
        assert_eq!(m.len(), 1);
        assert_eq!(m.approx_bytes(), after_first + 20);
        assert_eq!(
            m.insert(InternalKey::new(1, 10, b"a"), vec![0u8; 5]),
            Some(30)
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn approx_bytes_grows_with_new_entries() {
        let mut m = MemTable::new();
        let mut prev = m.approx_bytes();
        for ts in 0..5i64 {
            m.insert(InternalKey::new(1, ts, b""), vec![0u8; 100]);
            assert!(m.approx_bytes() > prev, "ts = {ts}");
            prev = m.approx_bytes();
        }
    }

    #[test]
    fn range_returns_sorted_subset() {
        let mut m = MemTable::new();
        for ts in [300i64, 100, 200] {
            m.insert(InternalKey::new(2, ts, b""), Vec::new());
        }
        m.insert(InternalKey::new(3, 100, b""), Vec::new());

        let got: Vec<i64> = m
            .range(2, 100, 300)
            .unwrap()
            .map(|(k, _)| k.timestamp_ms())
            .collect();
        assert_eq!(got, vec![100, 200]);
    }
}
