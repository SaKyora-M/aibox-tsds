//! AIBOX-TSDS —— 运行于边缘 AI 盒子的轻量级时序数据存储引擎。
//!
//! 存储架构：LSM-Tree（WAL + MemTable + SSTable）。
//! 对外契约见 `include/tsds.h`；本 crate 为纯 Rust 内核，`ffi` 模块导出 C ABI。
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod checksum;
pub mod compaction;
pub mod config;
pub mod db;
pub mod encoding;
pub mod error;
pub mod ffi;
pub mod iterator;
pub mod manifest;
pub mod memtable;
pub mod platform;
pub mod retention;
pub mod sstable;
pub mod wal;

/// 引擎版本号，取自 `Cargo.toml`。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
