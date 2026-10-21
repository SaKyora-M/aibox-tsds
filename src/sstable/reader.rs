//! SST 读取器：块索引定位 + 块内扫描。

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::encoding::InternalKey;
use crate::error::Result;
use crate::platform;

use super::{block, corrupt, decode_index, IndexEntry, FOOTER_LEN, MAGIC};

/// SST 读取器。
#[derive(Debug)]
pub struct SstableReader {
    path: PathBuf,
    file: File,
    index: Vec<IndexEntry>,
    file_size: u64,
}

impl SstableReader {
    /// 打开并加载块索引。
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        if file_size < FOOTER_LEN as u64 {
            return Err(corrupt("file smaller than footer"));
        }
        let footer_start = file_size - FOOTER_LEN as u64;

        let mut footer = [0u8; FOOTER_LEN];
        platform::read_at(&file, &mut footer, footer_start)?;
        if footer[12..16] != MAGIC {
            return Err(corrupt("bad footer magic"));
        }
        let index_offset = u64::from_le_bytes(footer[..8].try_into().expect("8 bytes"));
        let index_len = u32::from_le_bytes(footer[8..12].try_into().expect("4 bytes")) as usize;
        if index_offset + index_len as u64 > footer_start {
            return Err(corrupt("index out of bounds"));
        }

        let mut index_bytes = vec![0u8; index_len];
        platform::read_at(&file, &mut index_bytes, index_offset)?;
        let index = decode_index(&index_bytes)?;

        Ok(Self {
            path: path.to_path_buf(),
            file,
            index,
            file_size,
        })
    }

    /// 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 文件字节数。
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// 数据块数量。
    pub fn block_count(&self) -> usize {
        self.index.len()
    }

    /// 表中第一个键（读取首个数据块）。
    pub fn first_key(&self) -> Result<Option<InternalKey>> {
        let Some(entry) = self.index.first() else {
            return Ok(None);
        };
        let entries = self.read_block(entry)?;
        Ok(entries.first().map(|(key, _)| key.clone()))
    }

    /// 表中最后一个键（取自索引，无需读块）。
    pub fn last_key(&self) -> Option<&InternalKey> {
        self.index.last().map(|entry| &entry.last_key)
    }

    fn read_block(&self, entry: &IndexEntry) -> Result<Vec<(InternalKey, Vec<u8>)>> {
        let mut buf = vec![0u8; entry.len as usize];
        platform::read_at(&self.file, &mut buf, entry.offset)?;
        block::decode_block(&buf)
    }

    /// 精确查找。
    pub fn get(&self, key: &InternalKey) -> Result<Option<Vec<u8>>> {
        let position = self
            .index
            .partition_point(|entry| entry.last_key.as_bytes() < key.as_bytes());
        let Some(entry) = self.index.get(position) else {
            return Ok(None);
        };
        let entries = self.read_block(entry)?;
        let pos = entries.partition_point(|(candidate, _)| candidate.as_bytes() < key.as_bytes());
        match entries.get(pos) {
            Some((found, value)) if found.as_bytes() == key.as_bytes() => Ok(Some(value.clone())),
            _ => Ok(None),
        }
    }

    /// 顺序遍历全部条目。
    pub fn iter(&self) -> Result<SstableIter<'_>> {
        self.iter_range(None, None)
    }

    /// 按 `[lo, hi)` 顺序遍历条目。
    pub fn iter_range(
        &self,
        lo: Option<&InternalKey>,
        hi: Option<&InternalKey>,
    ) -> Result<SstableIter<'_>> {
        let start = match lo {
            Some(lo) => self
                .index
                .partition_point(|entry| entry.last_key.as_bytes() < lo.as_bytes()),
            None => 0,
        };
        Ok(SstableIter {
            reader: self,
            block_idx: start,
            entries: Vec::new(),
            pos: 0,
            lo: lo.cloned(),
            hi: hi.cloned(),
            done: false,
        })
    }
}

/// SST 顺序迭代器；产出的键值均为拥有所有权的副本。
pub struct SstableIter<'a> {
    reader: &'a SstableReader,
    block_idx: usize,
    entries: Vec<(InternalKey, Vec<u8>)>,
    pos: usize,
    lo: Option<InternalKey>,
    hi: Option<InternalKey>,
    done: bool,
}

impl Iterator for SstableIter<'_> {
    type Item = Result<(InternalKey, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            if self.pos < self.entries.len() {
                let (key, value) = self.entries[self.pos].clone();
                self.pos += 1;
                if let Some(lo) = &self.lo {
                    if key.as_bytes() < lo.as_bytes() {
                        continue;
                    }
                }
                if let Some(hi) = &self.hi {
                    if key.as_bytes() >= hi.as_bytes() {
                        self.done = true;
                        return None;
                    }
                }
                return Some(Ok((key, value)));
            }
            if self.block_idx >= self.reader.index.len() {
                self.done = true;
                return None;
            }
            match self.reader.read_block(&self.reader.index[self.block_idx]) {
                Ok(entries) => {
                    self.entries = entries;
                    self.pos = 0;
                    self.block_idx += 1;
                }
                Err(err) => {
                    self.done = true;
                    return Some(Err(err));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sstable::builder::SstableBuilder;
    use crate::sstable::SstableMeta;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tsds-sst-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("000001.sst")
    }

    fn build(path: &Path, count: i64, block_bytes: usize) -> SstableMeta {
        let mut builder = SstableBuilder::create(path, block_bytes).unwrap();
        for ts in 0..count {
            builder
                .add(&InternalKey::new(1, ts, b""), format!("v{ts}").as_bytes())
                .unwrap();
        }
        builder.finish().unwrap()
    }

    #[test]
    fn write_read_roundtrip() {
        let path = temp_path("roundtrip");
        let meta = build(&path, 100, 64);
        assert_eq!(meta.entry_count, 100);

        let reader = SstableReader::open(&path).unwrap();
        assert!(reader.block_count() > 1, "应产生多个数据块");
        let entries: Vec<_> = reader.iter().unwrap().map(|item| item.unwrap()).collect();
        assert_eq!(entries.len(), 100);
        assert_eq!(entries[42].0.timestamp_ms(), 42);
        assert_eq!(entries[42].1, b"v42".to_vec());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn get_hits_and_misses() {
        let path = temp_path("get");
        build(&path, 20, 64);
        let reader = SstableReader::open(&path).unwrap();
        assert_eq!(
            reader.get(&InternalKey::new(1, 7, b"")).unwrap(),
            Some(b"v7".to_vec())
        );
        assert_eq!(reader.get(&InternalKey::new(1, 20, b"")).unwrap(), None);
        assert_eq!(reader.get(&InternalKey::new(2, 7, b"")).unwrap(), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn iter_range_bounds() {
        let path = temp_path("range");
        build(&path, 20, 64);
        let reader = SstableReader::open(&path).unwrap();

        let lo = InternalKey::new(1, 5, b"");
        let hi = InternalKey::new(1, 10, b"");
        let got: Vec<i64> = reader
            .iter_range(Some(&lo), Some(&hi))
            .unwrap()
            .map(|item| item.unwrap().0.timestamp_ms())
            .collect();
        assert_eq!(got, vec![5, 6, 7, 8, 9]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn bad_magic_rejected() {
        let path = temp_path("magic");
        build(&path, 3, 64);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] = b'X';
        std::fs::write(&path, &bytes).unwrap();
        assert!(SstableReader::open(&path).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
