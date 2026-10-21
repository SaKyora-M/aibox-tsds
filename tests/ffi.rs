//! 第 9 步：C ABI 闭环测试（Rust 侧直接调用 `extern "C"` 函数）。

mod common;

use std::ffi::CString;
use std::ptr;

use common::TempDir;
use tsds::ffi::api::{
    tsds_batch_put, tsds_close, tsds_iter_free, tsds_iter_next, tsds_open, tsds_put, tsds_query,
};
use tsds::ffi::handle::{TsdsDb, TsdsItem, TSDS_ERR_INVALID_ARGUMENT, TSDS_OK};

fn open_db(tag: &str) -> (TempDir, CString, *mut TsdsDb) {
    let dir = TempDir::new(tag);
    let path = CString::new(dir.path().to_string_lossy().to_string()).unwrap();
    let db = unsafe { tsds_open(path.as_ptr(), 0) };
    assert!(!db.is_null(), "tsds_open 应成功");
    (dir, path, db)
}

fn put(db: *mut TsdsDb, channel: u32, timestamp_ms: i64, value: &[u8]) -> i32 {
    unsafe {
        tsds_put(
            db,
            channel,
            timestamp_ms,
            ptr::null(),
            0,
            value.as_ptr(),
            value.len(),
        )
    }
}

fn collect(db: *mut TsdsDb, channel: u32, start_ms: i64, end_ms: i64) -> Vec<(i64, Vec<u8>)> {
    let iter = unsafe { tsds_query(db, channel, start_ms, end_ms) };
    assert!(!iter.is_null(), "tsds_query 应成功");

    let mut out = Vec::new();
    let mut timestamp = 0i64;
    let mut value: *const u8 = ptr::null();
    let mut len = 0usize;
    loop {
        let rc = unsafe { tsds_iter_next(iter, &mut timestamp, &mut value, &mut len) };
        match rc {
            1 => out.push((
                timestamp,
                unsafe { std::slice::from_raw_parts(value, len) }.to_vec(),
            )),
            0 => break,
            other => panic!("tsds_iter_next 返回 {other}"),
        }
    }
    unsafe { tsds_iter_free(iter) };
    out
}

#[test]
fn ffi_roundtrip_put_query_close() {
    let (_dir, _path, db) = open_db("ffi-roundtrip");

    assert_eq!(put(db, 1, 100, b"v1"), TSDS_OK);
    assert_eq!(put(db, 1, 200, b"v2"), TSDS_OK);
    assert_eq!(put(db, 2, 300, b"v3"), TSDS_OK);

    let got = collect(db, 1, 0, i64::MAX);
    assert_eq!(got, vec![(100, b"v1".to_vec()), (200, b"v2".to_vec())]);
    assert_eq!(collect(db, 1, 200, i64::MAX), vec![(200, b"v2".to_vec())]);
    assert!(collect(db, 3, 0, i64::MAX).is_empty());

    unsafe { tsds_close(db) };
}

#[test]
fn ffi_rejects_null_and_bad_arguments() {
    assert_eq!(
        unsafe { tsds_put(ptr::null_mut(), 1, 0, ptr::null(), 0, ptr::null(), 0) },
        TSDS_ERR_INVALID_ARGUMENT
    );
    assert!(unsafe { tsds_query(ptr::null_mut(), 1, 0, 10) }.is_null());

    let mut timestamp = 0i64;
    let mut value: *const u8 = ptr::null();
    let mut len = 0usize;
    assert_eq!(
        unsafe { tsds_iter_next(ptr::null_mut(), &mut timestamp, &mut value, &mut len) },
        TSDS_ERR_INVALID_ARGUMENT
    );

    unsafe {
        tsds_close(ptr::null_mut());
        tsds_iter_free(ptr::null_mut());
    }

    let (_dir, _path, db) = open_db("ffi-bad-args");
    assert_eq!(
        unsafe { tsds_put(db, 1, 0, ptr::null(), 4, ptr::null(), 0) },
        TSDS_ERR_INVALID_ARGUMENT,
        "非零长度配 NULL 指针应被拒绝"
    );
    assert!(
        unsafe { tsds_query(db, 1, 100, 10) }.is_null(),
        "start > end 应返回 NULL"
    );
    unsafe { tsds_close(db) };
}

#[test]
fn ffi_iterator_pages_through_large_result() {
    let (_dir, _path, db) = open_db("ffi-paging");

    let values: Vec<Vec<u8>> = (0..600)
        .map(|ts| format!("v{ts:03}").into_bytes())
        .collect();
    let items: Vec<TsdsItem> = (0..600usize)
        .map(|index| TsdsItem {
            channel: 1,
            timestamp_ms: index as i64,
            key: ptr::null(),
            key_len: 0,
            value: values[index].as_ptr(),
            value_len: values[index].len(),
        })
        .collect();
    assert_eq!(
        unsafe { tsds_batch_put(db, items.as_ptr(), items.len()) },
        TSDS_OK
    );

    let got = collect(db, 1, 0, i64::MAX);
    assert_eq!(got.len(), 600, "应跨多页取完所有记录");
    assert_eq!(got.first().map(|(ts, _)| *ts), Some(0));
    assert_eq!(got.last().map(|(ts, _)| *ts), Some(599));
    assert_eq!(got[256].1, b"v256".to_vec());
    assert!(
        got.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "应按时间升序"
    );

    unsafe { tsds_close(db) };
}

#[test]
fn ffi_iterator_survives_close_until_freed() {
    let (_dir, _path, db) = open_db("ffi-iter-survives");
    for ts in [10i64, 20, 30] {
        assert_eq!(put(db, 1, ts, b"v"), TSDS_OK);
    }

    let iter = unsafe { tsds_query(db, 1, 0, i64::MAX) };
    assert!(!iter.is_null());
    unsafe { tsds_close(db) };

    let mut seen = 0;
    let mut timestamp = 0i64;
    let mut value: *const u8 = ptr::null();
    let mut len = 0usize;
    loop {
        let rc = unsafe { tsds_iter_next(iter, &mut timestamp, &mut value, &mut len) };
        if rc != 1 {
            assert_eq!(rc, 0);
            break;
        }
        seen += 1;
    }
    assert_eq!(seen, 3, "关闭句柄后迭代器应仍能读完（Arc 持有实例）");
    unsafe { tsds_iter_free(iter) };
}

#[test]
fn ffi_batch_put_roundtrip() {
    let (_dir, _path, db) = open_db("ffi-batch");

    let keys = [b"a".as_slice(), b"b".as_slice()];
    let values = [b"v1".as_slice(), b"v2".as_slice()];
    let items = [
        TsdsItem {
            channel: 1,
            timestamp_ms: 10,
            key: keys[0].as_ptr(),
            key_len: keys[0].len(),
            value: values[0].as_ptr(),
            value_len: values[0].len(),
        },
        TsdsItem {
            channel: 1,
            timestamp_ms: 20,
            key: keys[1].as_ptr(),
            key_len: keys[1].len(),
            value: values[1].as_ptr(),
            value_len: values[1].len(),
        },
    ];

    assert_eq!(unsafe { tsds_batch_put(db, items.as_ptr(), 2) }, TSDS_OK);
    assert_eq!(
        unsafe { tsds_batch_put(db, items.as_ptr(), 0) },
        TSDS_OK,
        "count = 0 应视为空批次成功"
    );
    assert_eq!(
        unsafe { tsds_batch_put(db, ptr::null(), 3) },
        TSDS_ERR_INVALID_ARGUMENT,
        "count > 0 但 items 为 NULL 应被拒绝"
    );

    let got = collect(db, 1, 0, i64::MAX);
    assert_eq!(got, vec![(10, b"v1".to_vec()), (20, b"v2".to_vec())]);

    unsafe { tsds_close(db) };
}

#[test]
fn ffi_reopen_sees_persisted_data() {
    let dir = TempDir::new("ffi-reopen");
    let path = CString::new(dir.path().to_string_lossy().to_string()).unwrap();

    {
        let db = unsafe { tsds_open(path.as_ptr(), 0) };
        assert!(!db.is_null());
        for ts in [10i64, 20, 30] {
            assert_eq!(put(db, 1, ts, b"v"), TSDS_OK);
        }
        unsafe { tsds_close(db) };
    }

    let db = unsafe { tsds_open(path.as_ptr(), 0) };
    assert!(!db.is_null());
    assert_eq!(collect(db, 1, 0, i64::MAX).len(), 3);
    unsafe { tsds_close(db) };
}
