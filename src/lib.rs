//! AIBOX-TSDS —— 运行于边缘 AI 盒子的轻量级时序数据存储引擎。
//!
//! 存储架构：LSM-Tree（WAL + MemTable + SSTable）。
//! 对外契约见 `include/tsds.h`（阶段 1 第 9 步产出）；本 crate 为纯 Rust 内核，
//! 第 9 步通过 `ffi` 模块导出 C ABI。
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;

/// 引擎版本号，取自 `Cargo.toml`。
///
/// 第 9 步会以 `tsds_version()` 的形式导出，供集成方确认加载的是哪个版本的库。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
