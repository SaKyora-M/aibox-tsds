//! 引擎门面：WAL 先落盘再进 MemTable；flush 落 L0。
//!
//! 合并（compaction）与生命周期维护（TTL / 容量）分别在 `db/compaction.rs`
//! 与 `db/maintenance.rs` 中实现，本文件只保留读写路径与实例生命周期。

mod compaction;
mod maintenance;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, MAX_VALUE_LEN};
use crate::encoding::InternalKey;
use crate::error::{Result, TsdsError};
use crate::iterator::{Cursor, MemCursor, MergeIterator};
use crate::manifest::{Manifest, SstableId, LEVEL0, LEVEL1};
use crate::memtable::MemTable;
use crate::retention;
use crate::sstable::{SstableBuilder, SstableMeta, SstableReader};
use crate::wal;

pub use compaction::CompactionReport;
pub use maintenance::Maintenance;

/// 批量写入的单条记录。
#[derive(Debug, Clone, Copy)]
pub struct BatchItem<'a> {
    /// 通道号。
    pub channel: u32,
    /// 时间戳（毫秒）。
    pub timestamp_ms: i64,
    /// 键。
    pub key: &'a [u8],
    /// 值。
    pub value: &'a [u8],
}

pub(crate) struct SstableEntry {
    pub(crate) level: u32,
    pub(crate) id: SstableId,
    pub(crate) reader: SstableReader,
}

/// 引擎实例。
pub struct Db {
    config: Config,
    path: PathBuf,
    mem: MemTable,
    wal: wal::Wal,
    manifest: Manifest,
    sstables: Vec<SstableEntry>,
    pending: Option<compaction::PendingCompaction>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (l0, l1) = self.level_counts();
        f.debug_struct("Db")
            .field("path", &self.path)
            .field("config", &self.config)
            .field("memtable_len", &self.mem.len())
            .field("l0", &l0)
            .field("l1", &l1)
            .field("compaction_pending", &self.pending.is_some())
            .finish()
    }
}

impl Db {
    /// 打开（不存在则创建）数据目录，使用系统时钟。
    pub fn open(path: &Path, config: Config) -> Result<Self> {
        Self::open_with_clock(path, config, Box::new(retention::now_ms))
    }

    /// 打开数据目录，并注入时钟（测试用）。
    pub fn open_with_clock(
        path: &Path,
        config: Config,
        clock: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Self> {
        let wal_dir = path.join("wal");
        fs::create_dir_all(&wal_dir)?;

        let mut mem = MemTable::new();
        let recovery = wal::replay(&wal_dir, |record| {
            mem.insert(record.key, record.value);
            Ok(())
        })?;

        let wal = wal::Wal::open(
            &wal_dir,
            recovery.last_segment_valid_len,
            config.wal_segment_bytes,
        )?;

        let manifest = Manifest::open(path)?;
        let mut sstables = Vec::new();
        for (level, id) in manifest.entries() {
            sstables.push(SstableEntry {
                level: *level,
                id: *id,
                reader: SstableReader::open(&manifest.path_for(*level, *id))?,
            });
        }

        Ok(Self {
            config,
            path: path.to_path_buf(),
            mem,
            wal,
            manifest,
            sstables,
            pending: None,
            clock,
        })
    }

    /// 数据目录。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 生效中的配置。
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
    }

    /// 读路径顺序：MemTable → L0（新到旧）→ L1。
    fn read_order(&self) -> impl Iterator<Item = &SstableEntry> + '_ {
        self.sstables
            .iter()
            .rev()
            .filter(|entry| entry.level == LEVEL0)
            .chain(
                self.sstables
                    .iter()
                    .rev()
                    .filter(|entry| entry.level == LEVEL1),
            )
    }

    /// 写入单条记录：先消化已完成的合并，再写 WAL 与 MemTable。
    pub fn put(&mut self, channel: u32, timestamp_ms: i64, key: &[u8], value: &[u8]) -> Result<()> {
        if value.len() > MAX_VALUE_LEN {
            return Err(TsdsError::InvalidArgument("value exceeds MAX_VALUE_LEN"));
        }
        self.poll_compaction()?;

        let internal_key = InternalKey::new(channel, timestamp_ms, key);
        self.wal.put(&internal_key, value)?;
        self.mem.insert(internal_key, value.to_vec());
        Ok(())
    }

    /// 批量写入：所有记录合并为一条 WAL 物理记录，只追加一次、fsync 一次。
    ///
    /// 返回 `0` 表示全部成功；返回 `N` 表示前 `N` 条已提交、第 `N+1` 条起未提交
    /// （目前唯一会中断的情况是 value 超过 `MAX_VALUE_LEN`）。
    pub fn batch_put(&mut self, items: &[BatchItem<'_>]) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        self.poll_compaction()?;

        let mut pairs: Vec<(InternalKey, Vec<u8>)> = Vec::with_capacity(items.len());
        for item in items {
            if item.value.len() > MAX_VALUE_LEN {
                break;
            }
            pairs.push((
                InternalKey::new(item.channel, item.timestamp_ms, item.key),
                item.value.to_vec(),
            ));
        }

        if pairs.is_empty() {
            return Err(TsdsError::InvalidArgument("all batch items invalid"));
        }
        let accepted = pairs.len();
        self.wal.put_batch(&pairs)?;
        for (key, value) in pairs {
            self.mem.insert(key, value);
        }

        Ok(if accepted == items.len() { 0 } else { accepted })
    }

    /// 精确读取：过期记录视为不存在。
    pub fn get(&self, channel: u32, timestamp_ms: i64, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if retention::is_expired(timestamp_ms, self.now(), self.config.default_ttl_ms) {
            return Ok(None);
        }
        let internal_key = InternalKey::new(channel, timestamp_ms, key);
        if let Some(value) = self.mem.get(&internal_key) {
            return Ok(Some(value.to_vec()));
        }
        for entry in self.read_order() {
            if let Some(value) = entry.reader.get(&internal_key)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// 时间范围查询 `[start_ms, end_ms)`，按时间升序流式返回。
    pub fn range(
        &self,
        channel: u32,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<impl Iterator<Item = Result<(i64, Vec<u8>)>> + '_> {
        if start_ms > end_ms {
            return Err(TsdsError::InvalidArgument("query range: start_ms > end_ms"));
        }
        let start_ms = match retention::alive_from_ms(self.now(), self.config.default_ttl_ms) {
            Some(alive) => start_ms.max(alive).min(end_ms),
            None => start_ms,
        };
        let (lo, hi) = crate::encoding::scan_bounds(channel, start_ms, end_ms)?;

        let mut cursors: Vec<Box<dyn Cursor + '_>> = Vec::with_capacity(self.sstables.len() + 1);
        cursors.push(Box::new(MemCursor::new(&self.mem, lo.clone(), hi.clone())));
        for entry in self.read_order() {
            cursors.push(Box::new(entry.reader.iter_range(Some(&lo), Some(&hi))?));
        }

        Ok(MergeIterator::new(cursors)
            .map(|item| item.map(|(key, value)| (key.timestamp_ms(), value))))
    }

    /// 分页扫描 `[start_ms, end_ms)`：从 `after` 之后开始，最多返回 `limit` 条。
    ///
    /// 供 FFI 迭代器使用：每次续扫一页，内存占用与结果集大小无关。
    pub fn scan(
        &self,
        channel: u32,
        start_ms: i64,
        end_ms: i64,
        after: Option<&InternalKey>,
        limit: usize,
    ) -> Result<Vec<(InternalKey, Vec<u8>)>> {
        if start_ms > end_ms {
            return Err(TsdsError::InvalidArgument("query range: start_ms > end_ms"));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let start_ms = match retention::alive_from_ms(self.now(), self.config.default_ttl_ms) {
            Some(alive) => start_ms.max(alive).min(end_ms),
            None => start_ms,
        };
        let (lo, hi) = crate::encoding::scan_bounds(channel, start_ms, end_ms)?;
        let lo = match after {
            Some(after) if after.as_bytes() > lo.as_bytes() => after.clone(),
            _ => lo,
        };

        let mut cursors: Vec<Box<dyn Cursor + '_>> = Vec::with_capacity(self.sstables.len() + 1);
        cursors.push(Box::new(MemCursor::new(&self.mem, lo.clone(), hi.clone())));
        for entry in self.read_order() {
            cursors.push(Box::new(entry.reader.iter_range(Some(&lo), Some(&hi))?));
        }

        let mut page = Vec::with_capacity(limit.min(1024));
        for item in MergeIterator::new(cursors) {
            let (key, value) = item?;
            if let Some(after) = after {
                if key.as_bytes() <= after.as_bytes() {
                    continue;
                }
            }
            page.push((key, value));
            if page.len() >= limit {
                break;
            }
        }
        Ok(page)
    }

    /// 内存表是否已达刷盘阈值。
    pub fn needs_flush(&self) -> bool {
        self.mem.approx_bytes() >= self.config.budget.memtable_bytes
    }

    /// 当前内存表记录数（不含 SST）。
    pub fn memtable_len(&self) -> usize {
        self.mem.len()
    }

    /// 已加载的 SST 数量。
    pub fn sstable_count(&self) -> usize {
        self.sstables.len()
    }

    /// 各层 SST 数量 `(L0, L1)`。
    pub fn level_counts(&self) -> (usize, usize) {
        let l0 = self.sstables.iter().filter(|e| e.level == LEVEL0).count();
        let l1 = self.sstables.iter().filter(|e| e.level == LEVEL1).count();
        (l0, l1)
    }

    /// 是否有尚未提交的后台合并。
    pub fn compaction_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// 当前占用估算：SST 文件字节 + 内存表估算。
    pub fn disk_bytes(&self) -> u64 {
        let sst: u64 = self
            .sstables
            .iter()
            .map(|entry| entry.reader.file_size())
            .sum();
        sst + self.mem.approx_bytes() as u64
    }

    /// 把 MemTable 落成 L0 SST，并按需触发后台合并。
    pub fn flush(&mut self) -> Result<Option<SstableMeta>> {
        self.poll_compaction()?;
        if self.mem.is_empty() {
            return Ok(None);
        }

        let now = self.now();
        let ttl = self.config.default_ttl_ms;
        let id = self.manifest.allocate();
        let path = self.manifest.path_for(LEVEL0, id);
        let mut builder = SstableBuilder::create(&path, self.config.sst_block_bytes)?;
        for (key, value) in self.mem.iter() {
            if retention::is_expired(key.timestamp_ms(), now, ttl) {
                continue;
            }
            builder.add(key, value)?;
        }

        if builder.entry_count() == 0 {
            drop(builder);
            let _ = fs::remove_file(&path);
            self.reset_wal()?;
            self.mem.clear();
            return Ok(None);
        }

        let meta = builder.finish()?;

        self.manifest.add(LEVEL0, id)?;
        self.reset_wal()?;
        self.mem.clear();
        self.sstables.push(SstableEntry {
            level: LEVEL0,
            id,
            reader: SstableReader::open(&path)?,
        });

        self.maybe_start_compaction()?;
        Ok(Some(meta))
    }

    fn reset_wal(&mut self) -> Result<()> {
        self.wal.rotate()?;
        self.wal.remove_segments_below()?;
        Ok(())
    }
}
