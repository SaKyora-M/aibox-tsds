//! SST 构建器：MemTable 有序落盘。

use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::encoding::InternalKey;
use crate::error::{Result, TsdsError};
use crate::platform;

use super::{corrupt, encode_index, IndexEntry, FOOTER_LEN, MAGIC};

/// 默认数据块大小（字节）。
pub const DEFAULT_BLOCK_BYTES: usize = 4096;

/// SST 落盘后的元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstableMeta {
    /// 文件路径。
    pub path: PathBuf,
    /// 文件字节数。
    pub file_size: u64,
    /// 条目数。
    pub entry_count: u64,
    /// 最小内部键。
    pub min_key: Option<InternalKey>,
    /// 最大内部键。
    pub max_key: Option<InternalKey>,
}

/// SST 构建器；`add` 必须按内部键严格递增调用。
#[derive(Debug)]
pub struct SstableBuilder {
    path: PathBuf,
    writer: BufWriter<std::fs::File>,
    offset: u64,
    block: Vec<u8>,
    block_last: Option<InternalKey>,
    last_key: Option<InternalKey>,
    index: Vec<IndexEntry>,
    block_bytes: usize,
    entry_count: u64,
    min_key: Option<InternalKey>,
    max_key: Option<InternalKey>,
}

impl SstableBuilder {
    /// 创建目标文件。
    pub fn create(path: &Path, block_bytes: usize) -> Result<Self> {
        if block_bytes == 0 {
            return Err(TsdsError::InvalidArgument("sst block_bytes must be > 0"));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: BufWriter::new(file),
            offset: 0,
            block: Vec::with_capacity(block_bytes),
            block_last: None,
            last_key: None,
            index: Vec::new(),
            block_bytes,
            entry_count: 0,
            min_key: None,
            max_key: None,
        })
    }

    /// 追加一条有序条目。
    pub fn add(&mut self, key: &InternalKey, value: &[u8]) -> Result<()> {
        if let Some(last) = &self.last_key {
            if key.as_bytes() <= last.as_bytes() {
                return Err(TsdsError::InvalidArgument(
                    "sst keys must be strictly ascending",
                ));
            }
        }
        super::block::encode_entry(&mut self.block, key, value);
        self.block_last = Some(key.clone());
        self.last_key = Some(key.clone());
        self.min_key.get_or_insert_with(|| key.clone());
        self.max_key = Some(key.clone());
        self.entry_count += 1;
        if self.block.len() >= self.block_bytes {
            self.flush_block()?;
        }
        Ok(())
    }

    /// 已写入的近似字节数。
    pub fn approximate_bytes(&self) -> u64 {
        self.offset + self.block.len() as u64
    }

    /// 已加入的条目数。
    pub fn entry_count(&self) -> u64 {
        self.entry_count
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.block.is_empty() {
            return Ok(());
        }
        let len = self.block.len() as u32;
        self.writer.write_all(&self.block)?;
        let last_key = self
            .block_last
            .take()
            .ok_or_else(|| corrupt("block has no last key"))?;
        self.index.push(IndexEntry {
            last_key,
            offset: self.offset,
            len,
        });
        self.offset += u64::from(len);
        self.block.clear();
        Ok(())
    }

    /// 写完索引与文件尾，fsync 并返回元信息。
    pub fn finish(mut self) -> Result<SstableMeta> {
        self.flush_block()?;

        let index_bytes = encode_index(&self.index);
        let index_offset = self.offset;
        let index_len = index_bytes.len() as u32;
        self.writer.write_all(&index_bytes)?;

        let mut footer = [0u8; FOOTER_LEN];
        footer[..8].copy_from_slice(&index_offset.to_le_bytes());
        footer[8..12].copy_from_slice(&index_len.to_le_bytes());
        footer[12..].copy_from_slice(&MAGIC);
        self.writer.write_all(&footer)?;
        self.writer.flush()?;

        let file = self.writer.into_inner().map_err(|err| err.into_error())?;
        file.sync_all()?;
        let file_size = file.metadata()?.len();
        drop(file);
        if let Some(parent) = self.path.parent() {
            platform::sync_dir(parent)?;
        }

        Ok(SstableMeta {
            path: self.path,
            file_size,
            entry_count: self.entry_count,
            min_key: self.min_key,
            max_key: self.max_key,
        })
    }
}
