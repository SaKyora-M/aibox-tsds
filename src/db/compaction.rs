//! L0→L1 合并驱动：计划、后台执行、前台提交。

use std::path::PathBuf;
use std::thread::JoinHandle;

use crate::compaction;
use crate::error::{Result, TsdsError};
use crate::manifest::{SstableId, LEVEL0, LEVEL1};
use crate::sstable::SstableReader;

use super::maintenance::remove_file_if_exists;
use super::{Db, SstableEntry};

/// 一次合并的结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// 输入 SST 数量。
    pub inputs: usize,
    /// 输出 SST 数量。
    pub outputs: usize,
    /// 丢弃的过期记录数。
    pub expired_dropped: u64,
}

struct CompactionPlan {
    ids: Vec<SstableId>,
    paths: Vec<PathBuf>,
}

struct JobResult {
    outcome: Result<compaction::MergeOutcome>,
}

pub(crate) struct PendingCompaction {
    handle: JoinHandle<JobResult>,
    input_ids: Vec<SstableId>,
}

impl Db {
    /// 同步执行一次 L0→L1 合并（不限速）。
    pub fn compact(&mut self) -> Result<CompactionReport> {
        let Some(plan) = self.plan_compaction()? else {
            return Ok(CompactionReport::default());
        };
        let mut limiter = compaction::RateLimiter::new(0);
        let block = self.config.sst_block_bytes;
        let target = self.config.l1_target_bytes;
        let now = self.now();
        let ttl = self.config.default_ttl_ms;

        let outcome = {
            let manifest = &mut self.manifest;
            let mut allocate = || {
                let id = manifest.allocate();
                (id, manifest.path_for(LEVEL1, id))
            };
            compaction::merge(
                &plan.paths,
                block,
                target,
                now,
                ttl,
                &mut limiter,
                &mut allocate,
            )?
        };

        let report = CompactionReport {
            inputs: plan.ids.len(),
            outputs: outcome.outputs.len(),
            expired_dropped: outcome.expired_dropped,
        };
        self.commit_compaction(&plan.ids, &outcome.outputs)?;
        Ok(report)
    }

    /// 消化已完成的合并；返回本次提交的作业数。
    pub fn poll_compaction(&mut self) -> Result<usize> {
        let finished = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.handle.is_finished());
        if !finished {
            return Ok(0);
        }
        let pending = self.pending.take().expect("checked above");
        let result = pending
            .handle
            .join()
            .map_err(|_| TsdsError::Internal("compaction thread panicked"))?;
        let outcome = result.outcome?;
        self.commit_compaction(&pending.input_ids, &outcome.outputs)?;
        Ok(1)
    }

    /// 等待后台合并完成并提交；返回是否真的等到了作业。
    pub fn wait_compaction(&mut self) -> Result<bool> {
        let Some(pending) = self.pending.take() else {
            return Ok(false);
        };
        let result = pending
            .handle
            .join()
            .map_err(|_| TsdsError::Internal("compaction thread panicked"))?;
        let outcome = result.outcome?;
        self.commit_compaction(&pending.input_ids, &outcome.outputs)?;
        Ok(true)
    }

    pub(super) fn maybe_start_compaction(&mut self) -> Result<()> {
        if !self.config.background_compaction || self.pending.is_some() {
            return Ok(());
        }
        if self.level_counts().0 < self.config.compaction_trigger_l0 {
            return Ok(());
        }
        let Some(plan) = self.plan_compaction()? else {
            return Ok(());
        };

        let allocator = self.manifest.allocator();
        let block = self.config.sst_block_bytes;
        let target = self.config.l1_target_bytes;
        let now = self.now();
        let ttl = self.config.default_ttl_ms;
        let rate = self.config.compaction_bytes_per_sec;
        let inputs = plan.paths;

        let handle = std::thread::Builder::new()
            .name("tsds-compaction".to_string())
            .spawn(move || {
                let mut limiter = compaction::RateLimiter::new(rate);
                let mut allocate = || {
                    let id = allocator.allocate();
                    (id, allocator.path_for(LEVEL1, id))
                };
                let outcome = compaction::merge(
                    &inputs,
                    block,
                    target,
                    now,
                    ttl,
                    &mut limiter,
                    &mut allocate,
                );
                JobResult { outcome }
            })?;

        self.pending = Some(PendingCompaction {
            handle,
            input_ids: plan.ids,
        });
        Ok(())
    }

    fn plan_compaction(&self) -> Result<Option<CompactionPlan>> {
        let l0_indexes: Vec<usize> = self
            .sstables
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.level == LEVEL0)
            .map(|(index, _)| index)
            .collect();
        if l0_indexes.is_empty() {
            return Ok(None);
        }

        let mut firsts = Vec::new();
        let mut lasts = Vec::new();
        for &index in &l0_indexes {
            let reader = &self.sstables[index].reader;
            if let Some(first) = reader.first_key()? {
                firsts.push(first);
            }
            if let Some(last) = reader.last_key() {
                lasts.push(last.clone());
            }
        }
        let Some(min_key) = firsts.into_iter().min() else {
            return Ok(None);
        };
        let max_key = lasts.into_iter().max().expect("l0 至少有一个键");

        let mut overlapping: Vec<usize> = Vec::new();
        for (index, entry) in self.sstables.iter().enumerate() {
            if entry.level != LEVEL1 {
                continue;
            }
            let first = entry.reader.first_key()?;
            let last = entry.reader.last_key().cloned();
            if let (Some(first), Some(last)) = (first, last) {
                let disjoint =
                    last.as_bytes() < min_key.as_bytes() || first.as_bytes() > max_key.as_bytes();
                if !disjoint {
                    overlapping.push(index);
                }
            }
        }

        let mut indexes: Vec<usize> = overlapping
            .iter()
            .chain(l0_indexes.iter())
            .copied()
            .collect();
        indexes.sort_unstable();
        indexes.dedup();

        let mut ids = Vec::with_capacity(indexes.len());
        let mut paths = Vec::with_capacity(indexes.len());
        for index in indexes {
            ids.push(self.sstables[index].id);
            paths.push(self.sstables[index].reader.path().to_path_buf());
        }
        Ok(Some(CompactionPlan { ids, paths }))
    }

    fn commit_compaction(
        &mut self,
        input_ids: &[SstableId],
        outputs: &[compaction::MergedSstable],
    ) -> Result<()> {
        // 先登记输出，再摘除输入：中间崩溃最多留下孤儿文件
        for output in outputs {
            self.manifest.add(LEVEL1, output.id)?;
        }
        for id in input_ids {
            self.manifest.remove(*id)?;
        }
        for id in input_ids {
            remove_file_if_exists(&self.manifest.path_for(LEVEL0, *id))?;
            remove_file_if_exists(&self.manifest.path_for(LEVEL1, *id))?;
        }

        self.sstables.retain(|entry| !input_ids.contains(&entry.id));
        for output in outputs {
            self.sstables.push(SstableEntry {
                level: LEVEL1,
                id: output.id,
                reader: SstableReader::open(&output.meta.path)?,
            });
        }
        Ok(())
    }
}
