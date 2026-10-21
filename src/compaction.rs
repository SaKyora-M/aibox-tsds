//! L0→L1 合并：多路归并、去重、丢过期、按目标大小分片、写限速。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::iterator::{Cursor, MergeIterator};
use crate::manifest::SstableId;
use crate::retention;
use crate::sstable::{SstableBuilder, SstableMeta, SstableReader};

/// 令牌桶写限速器；`bytes_per_sec == 0` 表示不限速。
#[derive(Debug)]
pub struct RateLimiter {
    bytes_per_sec: u64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    /// 以目标速率（字节/秒）创建，桶容量为 1 秒的量。
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec,
            tokens: bytes_per_sec as f64,
            last: Instant::now(),
        }
    }

    /// 记账 `bytes` 字节；超出当前配额时阻塞，直到速率约束满足。
    pub fn charge(&mut self, bytes: u64) {
        if self.bytes_per_sec == 0 {
            return;
        }
        self.refill();
        self.tokens -= bytes as f64;
        while self.tokens < 0.0 {
            let deficit = -self.tokens;
            let wait = deficit / self.bytes_per_sec as f64;
            std::thread::sleep(Duration::from_secs_f64(wait.min(0.05)));
            self.refill();
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let cap = self.bytes_per_sec as f64;
        self.tokens = (self.tokens + elapsed * cap).min(cap);
    }
}

/// 合并产出的单个 SST。
#[derive(Debug)]
pub struct MergedSstable {
    /// 文件 id。
    pub id: SstableId,
    /// 落盘元信息。
    pub meta: SstableMeta,
}

/// 一次合并的结果。
#[derive(Debug, Default)]
pub struct MergeOutcome {
    /// 产出的 SST。
    pub outputs: Vec<MergedSstable>,
    /// 丢弃的过期记录数。
    pub expired_dropped: u64,
}

/// 把输入（从旧到新）合并为新的 L1 文件。
///
/// * 同一内部键只保留最新来源的值；
/// * 过期记录直接丢弃；
/// * 输出达到 `target_bytes` 即切分新文件；
/// * 每写完一个文件按 `limiter` 限速。
pub fn merge(
    inputs: &[PathBuf],
    block_bytes: usize,
    target_bytes: u64,
    now_ms: i64,
    ttl_ms: Option<i64>,
    limiter: &mut RateLimiter,
    allocate_output: &mut dyn FnMut() -> (SstableId, PathBuf),
) -> Result<MergeOutcome> {
    let mut readers = Vec::with_capacity(inputs.len());
    for path in inputs {
        readers.push(SstableReader::open(path)?);
    }

    // 输入从旧到新，归并要求"最新来源优先级最高"，因此倒序建游标
    let mut cursors: Vec<Box<dyn Cursor>> = Vec::with_capacity(readers.len());
    for reader in readers.iter().rev() {
        cursors.push(Box::new(reader.iter()?));
    }

    let mut outputs = Vec::new();
    let mut expired_dropped = 0u64;
    let mut active: Option<(SstableId, SstableBuilder)> = None;

    for item in MergeIterator::new(cursors) {
        let (key, value) = item?;
        if retention::is_expired(key.timestamp_ms(), now_ms, ttl_ms) {
            expired_dropped += 1;
            continue;
        }

        if active.is_none() {
            let (id, path) = allocate_output();
            active = Some((id, SstableBuilder::create(&path, block_bytes)?));
        }

        let needs_split = {
            let (_, builder) = active.as_mut().expect("builder initialized");
            builder.add(&key, &value)?;
            builder.approximate_bytes() >= target_bytes
        };

        if needs_split {
            let (id, builder) = active.take().expect("builder present");
            let meta = builder.finish()?;
            limiter.charge(meta.file_size);
            outputs.push(MergedSstable { id, meta });
        }
    }

    if let Some((id, builder)) = active {
        let meta = builder.finish()?;
        limiter.charge(meta.file_size);
        outputs.push(MergedSstable { id, meta });
    }

    Ok(MergeOutcome {
        outputs,
        expired_dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_limiter_is_noop() {
        let mut limiter = RateLimiter::new(0);
        let start = Instant::now();
        limiter.charge(64 * 1024 * 1024);
        assert!(start.elapsed().as_millis() < 50);
    }

    #[test]
    fn limiter_throttles_when_bucket_exhausted() {
        let mut limiter = RateLimiter::new(1024 * 1024);
        let start = Instant::now();
        limiter.charge(512 * 1024);
        assert!(start.elapsed().as_millis() < 50);

        limiter.charge(2 * 1024 * 1024);
        assert!(start.elapsed().as_millis() >= 300, "超出桶容量后应被限速");
    }
}
