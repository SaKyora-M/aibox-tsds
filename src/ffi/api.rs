//! `include/tsds.h` 的 C ABI 实现。
//!
//! 七个导出函数：open / put / query / iter_next / iter_free / batch_put / close。
//! 所有函数都在 `ffi_guard` 内执行，panic 不会穿过 C 边界。

use std::collections::VecDeque;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::config::Config;
use crate::db::{BatchItem, Db};
use crate::error::TsdsError;

use super::handle::{
    error_code, ffi_guard, ffi_guard_ptr, ffi_guard_void, TsdsDb, TsdsItem, TsdsIter, ITER_PAGE,
    TSDS_ERR_INTERNAL, TSDS_ERR_INVALID_ARGUMENT, TSDS_OK,
};

/// # Safety
/// `path` 必须是有效的 NUL 结尾 C 字符串或 NULL。
unsafe fn path_from_c(path: *const c_char) -> Result<PathBuf, TsdsError> {
    if path.is_null() {
        return Err(TsdsError::InvalidArgument("path is null"));
    }
    let text = CStr::from_ptr(path)
        .to_str()
        .map_err(|_| TsdsError::InvalidArgument("path is not valid UTF-8"))?;
    Ok(PathBuf::from(text))
}

/// # Safety
/// `ptr` 要么为 NULL 且 `len == 0`，要么指向至少 `len` 字节的可读内存。
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], TsdsError> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(TsdsError::InvalidArgument(
            "null buffer with non-zero length",
        ));
    }
    Ok(std::slice::from_raw_parts(ptr, len))
}

fn lock_db(db: &TsdsDb) -> Option<MutexGuard<'_, Db>> {
    db.inner.lock().ok()
}

/// 打开（不存在则创建）实例；失败返回 NULL。
///
/// # Safety
/// `path` 必须是有效的 NUL 结尾 C 字符串。
#[no_mangle]
pub unsafe extern "C" fn tsds_open(path: *const c_char, mem_limit_mb: u32) -> *mut TsdsDb {
    ffi_guard_ptr(|| {
        let result = (|| -> Result<*mut TsdsDb, TsdsError> {
            let path = unsafe { path_from_c(path) }?;
            let config = Config::from_mem_limit_mb(mem_limit_mb)?;
            let db = Db::open(&path, config)?;
            Ok(Box::into_raw(Box::new(TsdsDb {
                inner: Arc::new(Mutex::new(db)),
            })))
        })();
        result.unwrap_or(ptr::null_mut())
    })
}

/// 写入单条记录；返回 0 表示已写入 WAL 并 fsync。
///
/// # Safety
/// `db` 必须是 `tsds_open` 返回且尚未关闭的句柄；`key` / `value` 需满足 [`bytes`] 的约定。
#[no_mangle]
pub unsafe extern "C" fn tsds_put(
    db: *mut TsdsDb,
    channel: u32,
    timestamp_ms: i64,
    key: *const u8,
    key_len: usize,
    value: *const u8,
    value_len: usize,
) -> i32 {
    ffi_guard(|| {
        let Some(handle) = (unsafe { db.as_ref() }) else {
            return TSDS_ERR_INVALID_ARGUMENT;
        };
        let key = match unsafe { bytes(key, key_len) } {
            Ok(slice) => slice,
            Err(err) => return error_code(&err),
        };
        let value = match unsafe { bytes(value, value_len) } {
            Ok(slice) => slice,
            Err(err) => return error_code(&err),
        };
        let Some(mut guard) = lock_db(handle) else {
            return TSDS_ERR_INTERNAL;
        };
        match guard.put(channel, timestamp_ms, key, value) {
            Ok(()) => TSDS_OK,
            Err(err) => error_code(&err),
        }
    })
}

/// 创建时间范围迭代器；失败返回 NULL。
///
/// # Safety
/// `db` 必须是 `tsds_open` 返回且尚未关闭的句柄。
#[no_mangle]
pub unsafe extern "C" fn tsds_query(
    db: *mut TsdsDb,
    channel: u32,
    start_ms: i64,
    end_ms: i64,
) -> *mut TsdsIter {
    ffi_guard_ptr(|| {
        let Some(handle) = (unsafe { db.as_ref() }) else {
            return ptr::null_mut();
        };
        if start_ms > end_ms {
            return ptr::null_mut();
        }
        let iter = TsdsIter {
            db: Arc::clone(&handle.inner),
            channel,
            start_ms,
            end_ms,
            buffer: VecDeque::new(),
            current: Vec::new(),
            last_key: None,
            exhausted: false,
        };
        Box::into_raw(Box::new(iter))
    })
}

/// 给迭代器续扫一页；返回 `Ok(true)` 表示缓冲区有新数据。
fn refill(iter: &mut TsdsIter) -> Result<bool, i32> {
    let page = {
        let Some(guard) = iter.db.lock().ok() else {
            return Err(TSDS_ERR_INTERNAL);
        };
        match guard.scan(
            iter.channel,
            iter.start_ms,
            iter.end_ms,
            iter.last_key.as_ref(),
            ITER_PAGE,
        ) {
            Ok(page) => page,
            Err(err) => return Err(error_code(&err)),
        }
    };

    if page.is_empty() {
        iter.exhausted = true;
        return Ok(false);
    }
    iter.last_key = page.last().map(|(key, _)| key.clone());
    iter.buffer = page
        .into_iter()
        .map(|(key, value)| (key.timestamp_ms(), value))
        .collect();
    Ok(true)
}

/// 前进一步：1 读到记录，0 结束，<0 出错。值的指针在下一次调用前保持有效。
///
/// # Safety
/// `it` 必须是 `tsds_query` 返回且尚未释放的句柄；三个输出参数必须可写。
#[no_mangle]
pub unsafe extern "C" fn tsds_iter_next(
    it: *mut TsdsIter,
    timestamp_ms_out: *mut i64,
    value_out: *mut *const u8,
    value_len_out: *mut usize,
) -> i32 {
    ffi_guard(|| {
        let Some(iter) = (unsafe { it.as_mut() }) else {
            return TSDS_ERR_INVALID_ARGUMENT;
        };
        if timestamp_ms_out.is_null() || value_out.is_null() || value_len_out.is_null() {
            return TSDS_ERR_INVALID_ARGUMENT;
        }

        loop {
            if let Some((timestamp, value)) = iter.buffer.pop_front() {
                iter.current = value;
                unsafe {
                    *timestamp_ms_out = timestamp;
                    *value_out = iter.current.as_ptr();
                    *value_len_out = iter.current.len();
                }
                return 1;
            }
            if iter.exhausted {
                return 0;
            }
            match refill(iter) {
                Ok(true) => continue,
                Ok(false) => return 0,
                Err(code) => return code,
            }
        }
    })
}

/// 释放迭代器；允许传 NULL。
///
/// # Safety
/// `it` 必须来自 `tsds_query`，且只能释放一次。
#[no_mangle]
pub unsafe extern "C" fn tsds_iter_free(it: *mut TsdsIter) {
    ffi_guard_void(|| {
        if !it.is_null() {
            drop(unsafe { Box::from_raw(it) });
        }
    });
}

/// 批量写入：0 全部成功，>0 前 N 条成功，<0 整体失败。
///
/// # Safety
/// `db` 必须是有效句柄；`items` 需指向至少 `count` 个有效条目。
#[no_mangle]
pub unsafe extern "C" fn tsds_batch_put(
    db: *mut TsdsDb,
    items: *const TsdsItem,
    count: usize,
) -> i32 {
    ffi_guard(|| {
        let Some(handle) = (unsafe { db.as_ref() }) else {
            return TSDS_ERR_INVALID_ARGUMENT;
        };
        if count == 0 {
            return TSDS_OK;
        }
        if items.is_null() {
            return TSDS_ERR_INVALID_ARGUMENT;
        }

        let raw = unsafe { std::slice::from_raw_parts(items, count) };
        let mut batch = Vec::with_capacity(count);
        for item in raw {
            let key = match unsafe { bytes(item.key, item.key_len) } {
                Ok(slice) => slice,
                Err(err) => return error_code(&err),
            };
            let value = match unsafe { bytes(item.value, item.value_len) } {
                Ok(slice) => slice,
                Err(err) => return error_code(&err),
            };
            batch.push(BatchItem {
                channel: item.channel,
                timestamp_ms: item.timestamp_ms,
                key,
                value,
            });
        }

        let Some(mut guard) = lock_db(handle) else {
            return TSDS_ERR_INTERNAL;
        };
        match guard.batch_put(&batch) {
            Ok(0) => TSDS_OK,
            Ok(done) => i32::try_from(done).unwrap_or(i32::MAX),
            Err(err) => error_code(&err),
        }
    })
}

/// 关闭实例；允许传 NULL，关闭后不得再使用该句柄。
///
/// # Safety
/// `db` 必须来自 `tsds_open`，且只能关闭一次。
#[no_mangle]
pub unsafe extern "C" fn tsds_close(db: *mut TsdsDb) {
    ffi_guard_void(|| {
        if !db.is_null() {
            drop(unsafe { Box::from_raw(db) });
        }
    });
}
