//! 第 8 步：批量写入（一次 WAL 追加、一次 fsync）。

mod common;

use common::TempDir;
use tsds::config::Config;
use tsds::db::{BatchItem, Db};
use tsds::wal::list_segments;

fn open(tag: &str) -> (TempDir, Db) {
    let dir = TempDir::new(tag);
    let db = Db::open(dir.path(), Config::default()).unwrap();
    (dir, db)
}

fn item<'a>(channel: u32, timestamp_ms: i64, key: &'a [u8], value: &'a [u8]) -> BatchItem<'a> {
    BatchItem {
        channel,
        timestamp_ms,
        key,
        value,
    }
}

#[test]
fn batch_put_writes_all_records() {
    let (_dir, mut db) = open("batch-all");
    let items = [
        item(1, 10, b"", b"a"),
        item(1, 20, b"", b"b"),
        item(2, 30, b"", b"c"),
    ];

    assert_eq!(db.batch_put(&items).unwrap(), 0);
    assert_eq!(db.get(1, 20, b"").unwrap(), Some(b"b".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 2);
    assert_eq!(db.range(2, 0, i64::MAX).unwrap().count(), 1);
}

#[test]
fn batch_put_uses_single_wal_record() {
    let (dir, mut db) = open("batch-single");
    let items = [item(1, 10, b"", b"a"), item(1, 20, b"", b"b")];
    db.batch_put(&items).unwrap();
    drop(db);

    let segments = list_segments(&dir.path().join("wal")).unwrap();
    assert_eq!(segments.len(), 1);
    let bytes = std::fs::read(&segments[0].1).unwrap();
    let (kind, _, next) = tsds::wal::record::decode_record(&bytes, 0).unwrap();
    assert_eq!(kind, tsds::wal::RecordKind::Batch);
    assert_eq!(next, bytes.len(), "两条记录应只占一条物理记录");
}

#[test]
fn batch_put_stops_before_oversized_value() {
    let (_dir, mut db) = open("batch-partial");
    let too_big = vec![0u8; 1024 * 1024 + 1];
    let items = [
        item(1, 10, b"", b"ok"),
        BatchItem {
            channel: 1,
            timestamp_ms: 20,
            key: b"",
            value: &too_big,
        },
        item(1, 30, b"", b"never"),
    ];

    assert_eq!(db.batch_put(&items).unwrap(), 1);
    assert_eq!(db.get(1, 10, b"").unwrap(), Some(b"ok".to_vec()));
    assert_eq!(db.get(1, 20, b"").unwrap(), None);
    assert_eq!(db.get(1, 30, b"").unwrap(), None);
}

#[test]
fn batch_put_rejects_all_invalid() {
    let (_dir, mut db) = open("batch-invalid");
    let too_big = vec![0u8; 1024 * 1024 + 1];
    let items = [BatchItem {
        channel: 1,
        timestamp_ms: 10,
        key: b"",
        value: &too_big,
    }];
    assert!(db.batch_put(&items).is_err());
}

#[test]
fn batch_put_replays_after_reopen() {
    let dir = TempDir::new("batch-replay");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        let items = [item(1, 10, b"", b"a"), item(1, 20, b"", b"b")];
        db.batch_put(&items).unwrap();
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 2);
    assert_eq!(db.get(1, 10, b"").unwrap(), Some(b"a".to_vec()));
}
