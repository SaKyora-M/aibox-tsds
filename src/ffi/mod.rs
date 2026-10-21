//! C ABI 导出层。
//!
//! 这一层只做三件事：指针/切片互转、句柄生命周期、panic 与错误码归一化。
//! 业务逻辑一律在 `crate::db` 里，本模块不含任何存储逻辑。
#![allow(unsafe_code)]

pub mod api;
pub mod handle;
