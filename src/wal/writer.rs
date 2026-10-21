//! WAL 追加写、fsync 与段轮转。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::encoding::InternalKey;
use crate::error::Result;
use crate::platform;

use super::record::{self, PutRecord, RecordKind};

/// 段文件名：`{段号:06}.log`。
pub fn segment_name(segment: u64) -> String {
    format!("{segment:06}.log")
}

/// 列出目录内全部 WAL 段，按段号升序返回 `(段号, 路径)`。
pub fn list_segments(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut segments = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(segments),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".log") {
            if let Ok(number) = stem.parse::<u64>() {
                segments.push((number, entry.path()));
            }
        }
    }
    segments.sort_by_key(|(number, _)| *number);
    Ok(segments)
}

/// 追加型 WAL 写入器。
#[derive(Debug)]
pub struct Wal {
    dir: PathBuf,
    file: File,
    segment: u64,
    written: u64,
    segment_bytes: u64,
}

impl Wal {
    /// 打开（或创建）WAL 目录。
    ///
    /// `truncate_last_to` 为最后一个段的有效长度：尾部撕裂写会让后续追加落在垃圾字节之后，
    /// 导致下次重放提前中断，因此必须在追加前截断到该长度。
    pub fn open(dir: &Path, truncate_last_to: Option<u64>, segment_bytes: u64) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let segments = list_segments(dir)?;

        let (segment, path) = match segments.last() {
            Some((number, path)) => (*number, path.clone()),
            None => (1, dir.join(segment_name(1))),
        };

        if let Some(valid_len) = truncate_last_to {
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)?;
            file.set_len(valid_len)?;
            file.sync_all()?;
            drop(file);
            platform::sync_dir(dir)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        let mut wal = Self {
            dir: dir.to_path_buf(),
            file,
            segment,
            written,
            segment_bytes,
        };
        if wal.written >= wal.segment_bytes {
            wal.rotate()?;
        }
        Ok(wal)
    }

    /// 当前段号。
    pub fn current_segment(&self) -> u64 {
        self.segment
    }

    /// 当前段已写字节数。
    pub fn written(&self) -> u64 {
        self.written
    }

    /// 追加一条 Put 记录并 fsync；返回即表示已持久化。
    pub fn put(&mut self, key: &InternalKey, value: &[u8]) -> Result<()> {
        let payload = PutRecord::new(key.clone(), value.to_vec()).encode_payload();
        let bytes = record::encode_record(RecordKind::Put, &payload)?;
        self.append(&bytes)
    }

    /// 追加一条批量记录并 fsync：一次追加、一次刷盘。
    pub fn put_batch(&mut self, items: &[(InternalKey, Vec<u8>)]) -> Result<()> {
        let payload = record::encode_batch_payload(items);
        let bytes = record::encode_record(RecordKind::Batch, &payload)?;
        self.append(&bytes)
    }

    /// 追加一条已编码的物理记录并 fsync。
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let len = bytes.len() as u64;
        if self.written > 0 && self.written + len > self.segment_bytes {
            self.rotate()?;
        }
        self.file.write_all(bytes)?;
        self.file.sync_data()?;
        self.written += len;
        Ok(())
    }

    /// 结束当前段并开启下一段。
    pub fn rotate(&mut self) -> Result<()> {
        self.file.sync_all()?;
        let next = self.segment + 1;
        let path = self.dir.join(segment_name(next));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        platform::sync_dir(&self.dir)?;
        self.file = file;
        self.segment = next;
        self.written = 0;
        Ok(())
    }

    /// 删除段号小于当前段的全部段文件（flush 提交后清理已持久化数据）。
    pub fn remove_segments_below(&mut self) -> Result<()> {
        let current = self.segment;
        for (number, path) in list_segments(&self.dir)? {
            if number < current {
                fs::remove_file(&path)?;
            }
        }
        platform::sync_dir(&self.dir)?;
        Ok(())
    }
}
