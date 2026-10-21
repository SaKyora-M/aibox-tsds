//! 生命周期维护：TTL 物理清理与容量淘汰。

use std::fs;
use std::path::Path;

use crate::error::{Result, TsdsError};
use crate::manifest::SstableId;
use crate::retention;
use crate::sstable::{SstableBuilder, SstableReader};

use super::{Db, SstableEntry};

/// 一次维护动作的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// 因含过期记录而重写的 SST 数量。
    pub rewritten_sstables: usize,
    /// 因容量超限而删除的最旧 SST 数量。
    pub evicted_sstables: usize,
}

impl Db {
    /// 物理清理过期记录：内存表丢弃、含过期记录的 SST 重写。
    pub fn purge_expired(&mut self) -> Result<usize> {
        let Some(ttl) = self.config.default_ttl_ms else {
            return Ok(0);
        };
        let now = self.now();

        self.mem
            .retain(|key| !retention::is_expired(key.timestamp_ms(), now, Some(ttl)));

        let mut targets = Vec::new();
        for entry in &self.sstables {
            if sst_contains_expired(&entry.reader, now, ttl)? {
                targets.push((entry.level, entry.id));
            }
        }

        let mut rewritten = 0;
        for (level, id) in targets {
            self.rewrite_sst(level, id, now, ttl)?;
            rewritten += 1;
        }
        Ok(rewritten)
    }

    /// 容量超限时按"最旧优先"删除 SST（至少保留一个）。
    pub fn enforce_capacity(&mut self) -> Result<usize> {
        let mut evicted = 0;
        while self.disk_bytes() > self.config.capacity_bytes && self.sstables.len() > 1 {
            let entry = self.sstables.remove(0);
            self.manifest.remove(entry.id)?;
            remove_file_if_exists(&self.manifest.path_for(entry.level, entry.id))?;
            evicted += 1;
        }
        Ok(evicted)
    }

    /// 执行一次完整维护：先收拢后台合并，再做 TTL 清理与容量淘汰。
    pub fn maintain(&mut self) -> Result<Maintenance> {
        while self.wait_compaction()? {}
        let rewritten_sstables = self.purge_expired()?;
        let evicted_sstables = self.enforce_capacity()?;
        Ok(Maintenance {
            rewritten_sstables,
            evicted_sstables,
        })
    }

    fn rewrite_sst(&mut self, level: u32, id: SstableId, now: i64, ttl: i64) -> Result<()> {
        let index = self
            .sstables
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(TsdsError::InvalidArgument("sst not loaded"))?;

        let new_id = self.manifest.allocate();
        let new_path = self.manifest.path_for(level, new_id);
        let mut builder = SstableBuilder::create(&new_path, self.config.sst_block_bytes)?;
        {
            let entry = &self.sstables[index];
            for item in entry.reader.iter()? {
                let (key, value) = item?;
                if retention::is_expired(key.timestamp_ms(), now, Some(ttl)) {
                    continue;
                }
                builder.add(&key, &value)?;
            }
        }
        builder.finish()?;

        self.manifest.add(level, new_id)?;
        self.manifest.remove(id)?;
        remove_file_if_exists(&self.manifest.path_for(level, id))?;

        self.sstables.remove(index);
        self.sstables.push(SstableEntry {
            level,
            id: new_id,
            reader: SstableReader::open(&new_path)?,
        });
        Ok(())
    }
}

fn sst_contains_expired(reader: &SstableReader, now: i64, ttl: i64) -> Result<bool> {
    for item in reader.iter()? {
        let (key, _value) = item?;
        if retention::is_expired(key.timestamp_ms(), now, Some(ttl)) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}
