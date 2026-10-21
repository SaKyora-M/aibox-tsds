//! 版本清单（MANIFEST）：按层级记录有效 SST 集合，原子更新。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::error::{Result, TsdsError};
use crate::platform;

const MAGIC: &str = "tsds-manifest-v2";
const LEGACY_MAGIC: &str = "tsds-manifest-v1";
const FILE_NAME: &str = "MANIFEST";

/// L0 层号。
pub const LEVEL0: u32 = 0;
/// L1 层号。
pub const LEVEL1: u32 = 1;

/// SST 文件标识（编号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SstableId(pub u64);

impl SstableId {
    /// 文件名，如 `000001.sst`。
    pub fn file_name(self) -> String {
        format!("{:06}.sst", self.0)
    }
}

fn level_path(root: &Path, level: u32, id: SstableId) -> PathBuf {
    root.join(format!("L{level}")).join(id.file_name())
}

/// 与清单共享同一 id 空间的线程安全分配器（供后台合并使用）。
#[derive(Debug, Clone)]
pub struct IdAllocator {
    next_id: Arc<AtomicU64>,
    root: PathBuf,
}

impl IdAllocator {
    /// 分配一个当前未被占用的 id。
    pub fn allocate(&self) -> SstableId {
        loop {
            let number = self.next_id.fetch_add(1, Ordering::SeqCst);
            let id = SstableId(number);
            let used = level_path(&self.root, LEVEL0, id).exists()
                || level_path(&self.root, LEVEL1, id).exists();
            if !used {
                return id;
            }
        }
    }

    /// 某个 id 在指定层的文件路径。
    pub fn path_for(&self, level: u32, id: SstableId) -> PathBuf {
        level_path(&self.root, level, id)
    }
}

/// 清单：`(层号, id)` 有序集合 + id 分配。
#[derive(Debug)]
pub struct Manifest {
    path: PathBuf,
    root: PathBuf,
    entries: Vec<(u32, SstableId)>,
    next_id: Arc<AtomicU64>,
}

impl Manifest {
    /// 打开（或初始化）清单。
    pub fn open(root: &Path) -> Result<Self> {
        let sst_root = root.join("sst");
        for level in [LEVEL0, LEVEL1] {
            fs::create_dir_all(sst_root.join(format!("L{level}")))?;
        }
        let path = root.join(FILE_NAME);
        let mut entries = Vec::new();
        let mut max_id = 0u64;

        if path.exists() {
            let text = fs::read_to_string(&path)?;
            let mut lines = text.lines();
            let magic = lines.next().unwrap_or_default();
            let legacy = magic == LEGACY_MAGIC;
            if !legacy && magic != MAGIC {
                return Err(TsdsError::Corrupted {
                    context: "manifest",
                    detail: "bad magic line".into(),
                });
            }
            for line in lines {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let (number, level) = if legacy {
                    (parse_id(line)?, LEVEL0)
                } else {
                    let mut parts = line.split_whitespace();
                    let number = parse_id(parts.next().unwrap_or_default())?;
                    let level = parts
                        .next()
                        .and_then(|raw| raw.parse::<u32>().ok())
                        .ok_or_else(|| TsdsError::Corrupted {
                            context: "manifest",
                            detail: format!("missing level: {line}"),
                        })?;
                    (number, level)
                };
                let id = SstableId(number);
                if level <= LEVEL1
                    && sst_root
                        .join(format!("L{level}"))
                        .join(id.file_name())
                        .exists()
                {
                    entries.push((level, id));
                    max_id = max_id.max(number);
                }
            }
        }

        Ok(Self {
            path,
            root: sst_root,
            entries,
            next_id: Arc::new(AtomicU64::new(max_id + 1)),
        })
    }

    /// 有效的 `(层号, id)` 集合，从旧到新。
    pub fn entries(&self) -> &[(u32, SstableId)] {
        &self.entries
    }

    /// 某一层的文件路径。
    pub fn path_for(&self, level: u32, id: SstableId) -> PathBuf {
        level_path(&self.root, level, id)
    }

    /// 分配一个未被占用的 id（跳过崩溃遗留的孤儿文件）。
    pub fn allocate(&self) -> SstableId {
        loop {
            let number = self.next_id.fetch_add(1, Ordering::SeqCst);
            let id = SstableId(number);
            let used = level_path(&self.root, LEVEL0, id).exists()
                || level_path(&self.root, LEVEL1, id).exists();
            if !used {
                return id;
            }
        }
    }

    /// 供后台合并线程使用的 id 分配器。
    pub fn allocator(&self) -> IdAllocator {
        IdAllocator {
            next_id: Arc::clone(&self.next_id),
            root: self.root.clone(),
        }
    }

    /// 登记新 SST 并原子落盘。
    pub fn add(&mut self, level: u32, id: SstableId) -> Result<()> {
        self.entries.push((level, id));
        self.persist()
    }

    /// 摘除登记（不删文件）并原子落盘。
    pub fn remove(&mut self, id: SstableId) -> Result<()> {
        self.entries.retain(|(_, existing)| *existing != id);
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            file.write_all(MAGIC.as_bytes())?;
            file.write_all(b"\n")?;
            for (level, id) in &self.entries {
                writeln!(file, "{:06} {}", id.0, level)?;
            }
            file.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        if let Some(parent) = self.path.parent() {
            platform::sync_dir(parent)?;
        }
        Ok(())
    }
}

fn parse_id(raw: &str) -> Result<u64> {
    raw.parse::<u64>().map_err(|_| TsdsError::Corrupted {
        context: "manifest",
        detail: format!("bad sst id: {raw}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tsds-manifest-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_allocate_and_reopen() {
        let root = temp_root("basic");
        let mut manifest = Manifest::open(&root).unwrap();
        assert!(manifest.entries().is_empty());

        let id = manifest.allocate();
        assert_eq!(id, SstableId(1));
        fs::write(manifest.path_for(LEVEL0, id), b"x").unwrap();
        manifest.add(LEVEL0, id).unwrap();

        let reopened = Manifest::open(&root).unwrap();
        assert_eq!(reopened.entries(), &[(LEVEL0, SstableId(1))]);
        assert_eq!(reopened.allocate(), SstableId(2));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn allocate_skips_orphan_file() {
        let root = temp_root("orphan");
        let manifest = Manifest::open(&root).unwrap();
        fs::write(manifest.path_for(LEVEL0, SstableId(1)), b"x").unwrap();
        assert_eq!(manifest.allocate(), SstableId(2));
        let _ = fs::remove_dir_all(&root);
    }
}
