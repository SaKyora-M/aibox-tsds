//! 引擎配置与内存预算拆分。

use crate::error::{Result, TsdsError};

/// 默认内存上限（MB），对应 `tsds_open` 传 0 的情况。
pub const DEFAULT_MEM_LIMIT_MB: u32 = 64;

/// 默认单个 WAL 段大小上限（字节）。
pub const DEFAULT_WAL_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;

/// 默认 SST 数据块大小（字节）。
pub const DEFAULT_SST_BLOCK_BYTES: usize = 4096;

/// 默认存储容量上限（字节）。
pub const DEFAULT_CAPACITY_BYTES: u64 = 512 * 1024 * 1024;

/// L1 输出文件的目标大小（字节）。
pub const DEFAULT_L1_TARGET_BYTES: u64 = 8 * 1024 * 1024;

/// L0 文件数达到该值即触发后台合并。
pub const DEFAULT_COMPACTION_TRIGGER_L0: usize = 4;

/// 后台合并的写限速（字节/秒）；0 表示不限速。
pub const DEFAULT_COMPACTION_BYTES_PER_SEC: u64 = 8 * 1024 * 1024;

/// 单条 value 的硬上限（字节）。方案书建议 ≤ 4KB，此处仅作安全阀。
pub const MAX_VALUE_LEN: usize = 1024 * 1024;

/// 容量超限时的淘汰策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvictionPolicy {
    /// 删除最旧数据（默认，兼容现有循环覆盖逻辑）。
    #[default]
    DeleteOldest,
    /// 按等级/类型分级保留。
    TieredRetention,
}

/// 内存预算拆分（各部分之和恒等于 `total_bytes`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget {
    /// 总上限（字节）。
    pub total_bytes: usize,
    /// 可写 MemTable 预算。
    pub memtable_bytes: usize,
    /// 落盘中只读 MemTable 预算（flush 期间不阻塞写入所必需）。
    pub immutable_memtable_bytes: usize,
    /// SST block cache 预算。
    pub block_cache_bytes: usize,
    /// 索引、布隆过滤器、迭代器与余量。
    pub misc_bytes: usize,
}

impl MemoryBudget {
    /// 按 `mem_limit_mb` 拆分预算；传 0 使用默认 64MB，下限 16MB。
    pub fn from_limit_mb(mem_limit_mb: u32) -> Result<Self> {
        let mb = if mem_limit_mb == 0 {
            DEFAULT_MEM_LIMIT_MB
        } else {
            mem_limit_mb
        };
        if mb < 16 {
            return Err(TsdsError::InvalidArgument("mem_limit_mb must be >= 16"));
        }
        let total_bytes = (mb as usize) * 1024 * 1024;
        let memtable_bytes = total_bytes / 4;
        let immutable_memtable_bytes = total_bytes / 4;
        let block_cache_bytes = total_bytes * 3 / 8;
        let misc_bytes =
            total_bytes - memtable_bytes - immutable_memtable_bytes - block_cache_bytes;
        Ok(Self {
            total_bytes,
            memtable_bytes,
            immutable_memtable_bytes,
            block_cache_bytes,
            misc_bytes,
        })
    }
}

/// 引擎配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// 内存预算。
    pub budget: MemoryBudget,
    /// 默认 TTL（毫秒）；`None` 表示不过期。
    pub default_ttl_ms: Option<i64>,
    /// 淘汰策略。
    pub eviction: EvictionPolicy,
    /// 单个 WAL 段的大小上限（字节），超过即轮转新段。
    pub wal_segment_bytes: u64,
    /// 单个 SST 数据块的目标大小（字节）。
    pub sst_block_bytes: usize,
    /// 存储容量上限（字节），超限时按最旧优先淘汰。
    pub capacity_bytes: u64,
    /// L1 输出文件的目标大小（字节），合并时据此切分。
    pub l1_target_bytes: u64,
    /// L0 文件数触发阈值。
    pub compaction_trigger_l0: usize,
    /// 后台合并写限速（字节/秒），0 为不限速。
    pub compaction_bytes_per_sec: u64,
    /// 是否启用后台合并。
    pub background_compaction: bool,
}

impl Config {
    /// 由 `tsds_open` 的 `mem_limit_mb` 参数构造配置。
    pub fn from_mem_limit_mb(mem_limit_mb: u32) -> Result<Self> {
        Ok(Self {
            budget: MemoryBudget::from_limit_mb(mem_limit_mb)?,
            default_ttl_ms: None,
            eviction: EvictionPolicy::default(),
            wal_segment_bytes: DEFAULT_WAL_SEGMENT_BYTES,
            sst_block_bytes: DEFAULT_SST_BLOCK_BYTES,
            capacity_bytes: DEFAULT_CAPACITY_BYTES,
            l1_target_bytes: DEFAULT_L1_TARGET_BYTES,
            compaction_trigger_l0: DEFAULT_COMPACTION_TRIGGER_L0,
            compaction_bytes_per_sec: DEFAULT_COMPACTION_BYTES_PER_SEC,
            background_compaction: true,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_mem_limit_mb(0).expect("default config is always valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_splits_64mb() {
        let b = MemoryBudget::from_limit_mb(0).unwrap();
        assert_eq!(b.total_bytes, 64 * 1024 * 1024);
        assert_eq!(b.memtable_bytes, 16 * 1024 * 1024);
        assert_eq!(b.immutable_memtable_bytes, 16 * 1024 * 1024);
        assert_eq!(b.block_cache_bytes, 24 * 1024 * 1024);
        assert_eq!(b.misc_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn budget_parts_sum_to_total() {
        for mb in [16u32, 64, 100, 512] {
            let b = MemoryBudget::from_limit_mb(mb).unwrap();
            assert_eq!(
                b.memtable_bytes + b.immutable_memtable_bytes + b.block_cache_bytes + b.misc_bytes,
                b.total_bytes,
                "mb = {mb}"
            );
        }
    }

    #[test]
    fn rejects_too_small_limit() {
        assert!(MemoryBudget::from_limit_mb(8).is_err());
    }

    #[test]
    fn default_config_uses_4mb_segments() {
        assert_eq!(
            Config::default().wal_segment_bytes,
            DEFAULT_WAL_SEGMENT_BYTES
        );
    }
}
