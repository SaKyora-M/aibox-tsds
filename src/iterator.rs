//! 查询迭代器：统一游标抽象 + 多路归并。

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::encoding::InternalKey;
use crate::error::{Result, TsdsError};
use crate::memtable::MemTable;
use crate::sstable::SstableIter;

/// 游标：按内部键升序产出条目，供归并使用。
pub trait Cursor {
    /// 取下一条；`None` 表示该来源结束。
    fn next_entry(&mut self) -> Option<Result<(InternalKey, Vec<u8>)>>;
}

/// MemTable 游标（借用内存表，不复制整表）。
pub struct MemCursor<'a> {
    inner: Box<dyn Iterator<Item = (&'a InternalKey, &'a [u8])> + 'a>,
}

impl<'a> MemCursor<'a> {
    /// 构造 `[lo, hi)` 区间的 MemTable 游标。
    pub fn new(mem: &'a MemTable, lo: InternalKey, hi: InternalKey) -> Self {
        Self {
            inner: Box::new(mem.iter_range(lo, hi)),
        }
    }
}

impl Cursor for MemCursor<'_> {
    fn next_entry(&mut self) -> Option<Result<(InternalKey, Vec<u8>)>> {
        self.inner
            .next()
            .map(|(key, value)| Ok((key.clone(), value.to_vec())))
    }
}

impl Cursor for SstableIter<'_> {
    fn next_entry(&mut self) -> Option<Result<(InternalKey, Vec<u8>)>> {
        Iterator::next(self)
    }
}

/// 堆元素：键 + 值 + 来源优先级。
struct HeapItem {
    key: InternalKey,
    value: Vec<u8>,
    priority: usize,
    cursor: usize,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for HeapItem {}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap 是最大堆，反转键比较使最小键位于堆顶；
        // 同键时 priority 更小（来源更新）者优先弹出，实现"新值覆盖旧值"。
        other
            .key
            .cmp(&self.key)
            .then_with(|| other.priority.cmp(&self.priority))
    }
}

/// 多路归并迭代器：键升序输出，同键只保留优先级最高（最新）的一条。
pub struct MergeIterator<'a> {
    cursors: Vec<Box<dyn Cursor + 'a>>,
    heap: BinaryHeap<HeapItem>,
    last_key: Option<InternalKey>,
    pending_error: Option<TsdsError>,
}

impl<'a> MergeIterator<'a> {
    /// 由若干游标构造；下标越小优先级越高（0 为 MemTable，最新）。
    pub fn new(mut cursors: Vec<Box<dyn Cursor + 'a>>) -> Self {
        let mut heap = BinaryHeap::new();
        let mut pending_error = None;

        for (index, cursor) in cursors.iter_mut().enumerate() {
            match cursor.next_entry() {
                Some(Ok((key, value))) => heap.push(HeapItem {
                    key,
                    value,
                    priority: index,
                    cursor: index,
                }),
                Some(Err(err)) => {
                    pending_error = Some(err);
                    break;
                }
                None => {}
            }
        }

        Self {
            cursors,
            heap,
            last_key: None,
            pending_error,
        }
    }
}

impl Iterator for MergeIterator<'_> {
    type Item = Result<(InternalKey, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(err) = self.pending_error.take() {
            return Some(Err(err));
        }

        loop {
            let item = self.heap.pop()?;
            let HeapItem {
                key,
                value,
                priority,
                cursor,
            } = item;

            if let Some(next) = self.cursors[cursor].next_entry() {
                match next {
                    Ok((next_key, next_value)) => self.heap.push(HeapItem {
                        key: next_key,
                        value: next_value,
                        priority,
                        cursor,
                    }),
                    Err(err) => self.pending_error = Some(err),
                }
            }

            if self.last_key.as_ref() == Some(&key) {
                continue;
            }
            self.last_key = Some(key.clone());
            return Some(Ok((key, value)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct VecCursor {
        items: std::vec::IntoIter<(InternalKey, Vec<u8>)>,
    }

    impl VecCursor {
        fn new(items: Vec<(InternalKey, Vec<u8>)>) -> Self {
            Self {
                items: items.into_iter(),
            }
        }
    }

    impl Cursor for VecCursor {
        fn next_entry(&mut self) -> Option<Result<(InternalKey, Vec<u8>)>> {
            self.items.next().map(Ok)
        }
    }

    fn key(ts: i64) -> InternalKey {
        InternalKey::new(1, ts, b"")
    }

    #[test]
    fn merge_is_sorted_and_deduped() {
        let newer = VecCursor::new(vec![
            (key(2), b"new-2".to_vec()),
            (key(3), b"new-3".to_vec()),
        ]);
        let older = VecCursor::new(vec![
            (key(1), b"old-1".to_vec()),
            (key(2), b"old-2".to_vec()),
            (key(4), b"old-4".to_vec()),
        ]);

        let cursors: Vec<Box<dyn Cursor>> = vec![Box::new(newer), Box::new(older)];
        let got: Vec<(i64, Vec<u8>)> = MergeIterator::new(cursors)
            .map(|item| item.unwrap())
            .map(|(k, v)| (k.timestamp_ms(), v))
            .collect();

        assert_eq!(
            got,
            vec![
                (1, b"old-1".to_vec()),
                (2, b"new-2".to_vec()),
                (3, b"new-3".to_vec()),
                (4, b"old-4".to_vec()),
            ]
        );
    }

    #[test]
    fn merge_of_empty_cursors_is_empty() {
        let cursors: Vec<Box<dyn Cursor>> = vec![
            Box::new(VecCursor::new(Vec::new())),
            Box::new(VecCursor::new(Vec::new())),
        ];
        assert_eq!(MergeIterator::new(cursors).count(), 0);
    }
}
