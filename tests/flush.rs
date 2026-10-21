//! 第 4 步：MemTable 落盘、manifest 持久化、WAL 清理、读路径合并。

mod common;

use common::TempDir;
use tsds::config::Config;
use tsds::db::Db;
use tsds::wal::list_segments;

fn config(block_bytes: usize) -> Config {
    Config {
        sst_block_bytes: block_bytes,
        ..Config::default()
    }
}

#[test]
fn flush_persists_to_sst_and_clears_memtable() {
    let dir = TempDir::new("flush-basic");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"a", b"v1").unwrap();
        db.put(1, 200, b"b", b"v2").unwrap();

        let meta = db.flush().unwrap().expect("应产出 SST");
        assert_eq!(meta.entry_count, 2);
        assert_eq!(db.memtable_len(), 0);
        assert_eq!(db.sstable_count(), 1);
        assert_eq!(db.get(1, 100, b"a").unwrap(), Some(b"v1".to_vec()));
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.get(1, 200, b"b").unwrap(), Some(b"v2".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 2);
}

#[test]
fn flush_twice_and_reopen_keeps_all() {
    let dir = TempDir::new("flush-twice");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"a", b"v1").unwrap();
        db.flush().unwrap();
        db.put(1, 200, b"b", b"v2").unwrap();
        db.flush().unwrap();
        assert_eq!(db.sstable_count(), 2);
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.sstable_count(), 2);
    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got, vec![100, 200]);
}

#[test]
fn newer_write_wins_over_flushed_value() {
    let dir = TempDir::new("flush-newer");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"k", b"old").unwrap();
        db.flush().unwrap();
        db.put(1, 100, b"k", b"new").unwrap();
        assert_eq!(db.get(1, 100, b"k").unwrap(), Some(b"new".to_vec()));
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.get(1, 100, b"k").unwrap(), Some(b"new".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
}

#[test]
fn multi_block_sst_roundtrip() {
    let dir = TempDir::new("flush-blocks");
    let cfg = config(64);
    {
        let mut db = Db::open(dir.path(), cfg).unwrap();
        for ts in 0..50i64 {
            db.put(1, ts, b"", format!("value-{ts:02}").as_bytes())
                .unwrap();
        }
        let meta = db.flush().unwrap().unwrap();
        assert!(meta.file_size > 0);
    }

    let db = Db::open(dir.path(), cfg).unwrap();
    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got, (0..50).collect::<Vec<_>>());
    assert_eq!(db.get(1, 37, b"").unwrap(), Some(b"value-37".to_vec()));
    assert_eq!(db.get(1, 50, b"").unwrap(), None);
}

#[test]
fn flush_truncates_wal_to_single_empty_segment() {
    let dir = TempDir::new("flush-wal");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        for ts in 0..5i64 {
            db.put(1, ts, b"", b"v").unwrap();
        }
        db.flush().unwrap();

        let segments = list_segments(&dir.path().join("wal")).unwrap();
        assert_eq!(segments.len(), 1, "flush 后应只剩当前段: {segments:?}");
        assert_eq!(std::fs::metadata(&segments[0].1).unwrap().len(), 0);
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 5);
}

#[test]
fn flush_on_empty_memtable_is_noop() {
    let dir = TempDir::new("flush-noop");
    let mut db = Db::open(dir.path(), Config::default()).unwrap();
    assert!(db.flush().unwrap().is_none());
    assert_eq!(db.sstable_count(), 0);
}
