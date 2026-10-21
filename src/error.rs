//! 统一错误类型。所有模块返回 [`Result`]，第 9 步在 `ffi` 层映射为 C 整数错误码。

use std::fmt;
use std::io;

/// 引擎统一结果类型。
pub type Result<T> = std::result::Result<T, TsdsError>;

/// 引擎可能产生的全部错误。
#[derive(Debug)]
pub enum TsdsError {
    /// 底层 IO 失败（含 fsync 失败）。
    Io(io::Error),
    /// 磁盘数据校验失败或格式不符（WAL 撕裂、SST 损坏、manifest 异常）。
    Corrupted {
        /// 出错的逻辑位置，如 `"wal.replay"`、`"sst.block"`。
        context: &'static str,
        /// 供日志与排障的具体描述。
        detail: String,
    },
    /// 调用方传入非法参数（NULL 指针、区间反向、value 超限等）。
    InvalidArgument(&'static str),
    /// 本次写入会超出 `mem_limit_mb` 预算。
    OutOfMemoryBudget {
        /// 本次需要追加的字节数。
        requested_bytes: usize,
        /// 已配置的上限（字节）。
        limit_bytes: usize,
    },
    /// 句柄已关闭或已释放后继续使用。
    Closed,
    /// 引擎内部不变量被破坏（如后台线程 panic）。
    Internal(&'static str),
}

impl fmt::Display for TsdsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TsdsError::Io(e) => write!(f, "io error: {e}"),
            TsdsError::Corrupted { context, detail } => {
                write!(f, "corrupted data at {context}: {detail}")
            }
            TsdsError::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
            TsdsError::OutOfMemoryBudget {
                requested_bytes,
                limit_bytes,
            } => write!(
                f,
                "memory budget exceeded: requested {requested_bytes} bytes, limit {limit_bytes} bytes"
            ),
            TsdsError::Closed => write!(f, "engine handle already closed"),
            TsdsError::Internal(what) => write!(f, "internal error: {what}"),
        }
    }
}

impl std::error::Error for TsdsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TsdsError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for TsdsError {
    fn from(e: io::Error) -> Self {
        TsdsError::Io(e)
    }
}
