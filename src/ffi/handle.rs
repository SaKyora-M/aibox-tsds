//! FFI 句柄、错误码与 panic 防护。

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use crate::db::Db;
use crate::encoding::InternalKey;
use crate::error::TsdsError;

/// 迭代器单页条数（每次续扫从内核取这么多条）。
pub(crate) const ITER_PAGE: usize = 256;

/// 成功。
pub const TSDS_OK: i32 = 0;
/// 参数非法（NULL 指针、区间反向、value 超限、路径非法）。
pub const TSDS_ERR_INVALID_ARGUMENT: i32 = -1;
/// IO / 磁盘错误。
pub const TSDS_ERR_IO: i32 = -2;
/// 数据损坏（WAL CRC、MANIFEST、SST 校验失败）。
pub const TSDS_ERR_CORRUPTED: i32 = -3;
/// 内部错误（后台线程 panic、句柄已关闭）。
pub const TSDS_ERR_INTERNAL: i32 = -4;

/// 与 `tsds.h` 中 `tsds_item` 内存布局一致的批量条目。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TsdsItem {
    /// 通道号。
    pub channel: u32,
    /// 时间戳（毫秒）。
    pub timestamp_ms: i64,
    /// 键指针。
    pub key: *const u8,
    /// 键长度。
    pub key_len: usize,
    /// 值指针。
    pub value: *const u8,
    /// 值长度。
    pub value_len: usize,
}

/// C 侧看到的数据库句柄。
pub struct TsdsDb {
    pub(crate) inner: Arc<Mutex<Db>>,
}

/// C 侧看到的迭代器句柄。
///
/// 持有 `Arc<Mutex<Db>>` 的克隆：即使 `tsdb_close` 已调用，迭代器仍能读完（实例在
/// `Arc` 里存活）；每次续扫都重新加锁，与写入互不别名。
pub struct TsdsIter {
    pub(crate) db: Arc<Mutex<Db>>,
    pub(crate) channel: u32,
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    pub(crate) buffer: VecDeque<(i64, Vec<u8>)>,
    pub(crate) current: Vec<u8>,
    pub(crate) last_key: Option<InternalKey>,
    pub(crate) exhausted: bool,
}

/// 把内部错误映射为 C 错误码。
pub fn error_code(err: &TsdsError) -> i32 {
    match err {
        TsdsError::InvalidArgument(_) => TSDS_ERR_INVALID_ARGUMENT,
        TsdsError::Io(_) => TSDS_ERR_IO,
        TsdsError::Corrupted { .. } => TSDS_ERR_CORRUPTED,
        TsdsError::OutOfMemoryBudget { .. } | TsdsError::Closed | TsdsError::Internal(_) => {
            TSDS_ERR_INTERNAL
        }
    }
}

/// 在 C 边界内执行并拦截 panic，把 panic 归一化为内部错误码。
pub fn ffi_guard<F: FnOnce() -> i32>(f: F) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => TSDS_ERR_INTERNAL,
    }
}

/// 在 C 边界内执行并拦截 panic（无返回值版本）。
pub fn ffi_guard_void<F: FnOnce()>(f: F) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

/// 在 C 边界内执行并拦截 panic；panic 时返回空指针。
pub fn ffi_guard_ptr<T, F: FnOnce() -> *mut T>(f: F) -> *mut T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(ptr) => ptr,
        Err(_) => std::ptr::null_mut(),
    }
}
