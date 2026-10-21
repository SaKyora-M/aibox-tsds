//! 集成测试共用的临时目录（零依赖，Drop 时删除）。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 进程内唯一、Drop 时删除的临时目录。
pub struct TempDir {
    path: PathBuf,
}

/// 可控时钟，供 TTL / 合并相关测试使用。
pub struct TestClock {
    cell: Arc<AtomicI64>,
}

impl TempDir {
    /// 创建临时目录。
    pub fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tsds-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// 目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl TestClock {
    /// 以指定毫秒值起始。
    pub fn new(start: i64) -> Self {
        Self {
            cell: Arc::new(AtomicI64::new(start)),
        }
    }

    /// 拨动时钟。
    pub fn set(&self, value: i64) {
        self.cell.store(value, Ordering::Relaxed);
    }

    /// 交给 `Db::open_with_clock` 的闭包。
    pub fn handle(&self) -> Box<dyn Fn() -> i64 + Send + Sync> {
        let cell = Arc::clone(&self.cell);
        Box::new(move || cell.load(Ordering::Relaxed))
    }
}
