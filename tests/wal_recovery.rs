//! 第 3 步：WAL 重放、撕裂写截断、CRC 损坏、段轮转。

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::TempDir;
use tsds::config::Config;
use tsds::db::Db;
use tsds::wal::list_segments;

fn config(segment_bytes: u64) -> Config {
    Config {
        wal_segment_bytes: segment_bytes,
        ..Config::default()
    }
}

fn last_segment(dir: &Path) -> PathBuf {
    list_segments(&dir.join("wal"))
        .unwrap()
        .last()
        .unwrap()
        .1
        .clone()
}

#[test]
fn reopen_replays_confirmed_writes() {
    let dir = TempDir::new("wal-replay");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"a", b"v1").unwrap();
        db.put(1, 200, b"b", b"v2").unwrap();
        db.put(2, 300, b"c", b"v3").unwrap();
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.get(1, 100, b"a").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(db.get(2, 300, b"c").unwrap(), Some(b"v3".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 2);
}

#[test]
fn torn_tail_is_truncated_and_later_writes_survive() {
    let dir = TempDir::new("wal-torn");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"a", b"v1").unwrap();
    }

    let path = last_segment(dir.path());
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(&[0xAB; 5]);
    fs::write(&path, &bytes).unwrap();

    let mut db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.get(1, 100, b"a").unwrap(), Some(b"v1".to_vec()));
    db.put(1, 200, b"b", b"v2").unwrap();
    drop(db);

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.get(1, 100, b"a").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(db.get(1, 200, b"b").unwrap(), Some(b"v2".to_vec()));
}

#[test]
fn corrupted_tail_record_is_dropped_earlier_kept() {
    let dir = TempDir::new("wal-corrupt");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"a", b"v1").unwrap();
        db.put(1, 200, b"b", b"v2").unwrap();
    }

    let path = last_segment(dir.path());
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    fs::write(&path, &bytes).unwrap();

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.get(1, 100, b"a").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(db.get(1, 200, b"b").unwrap(), None);
}

#[test]
fn segments_rotate_and_replay_across_files() {
    let dir = TempDir::new("wal-rotate");
    let cfg = config(64);
    {
        let mut db = Db::open(dir.path(), cfg).unwrap();
        for ts in 0..10i64 {
            db.put(1, ts, b"", format!("value-{ts}").as_bytes())
                .unwrap();
        }
    }

    let segments = list_segments(&dir.path().join("wal")).unwrap();
    assert!(segments.len() > 1, "应轮转出多个段: {segments:?}");

    let db = Db::open(dir.path(), cfg).unwrap();
    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got, (0..10).collect::<Vec<_>>());
}
