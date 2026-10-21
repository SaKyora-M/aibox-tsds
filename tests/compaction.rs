//! 第 7a 步：L0→L1 合并、去重、丢过期、按目标大小分片。

mod common;

use common::TempDir;
use tsds::config::Config;
use tsds::db::Db;

fn with_target(target_bytes: u64) -> Config {
    Config {
        l1_target_bytes: target_bytes,
        ..Config::default()
    }
}

#[test]
fn compaction_moves_l0_into_l1_and_keeps_data() {
    let dir = TempDir::new("compact-basic");
    let mut db = Db::open(dir.path(), Config::default()).unwrap();

    for round in 0..3i64 {
        for ts in 0..5i64 {
            db.put(
                1,
                round * 100 + ts,
                b"",
                format!("v{}", round * 100 + ts).as_bytes(),
            )
            .unwrap();
        }
        db.flush().unwrap();
    }
    assert_eq!(db.level_counts(), (3, 0));

    let report = db.compact().unwrap();
    assert_eq!(report.inputs, 3);
    assert_eq!(report.outputs, 1);
    assert_eq!(db.level_counts(), (0, 1));

    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got.len(), 15);
    assert_eq!(got.first(), Some(&0));
    assert_eq!(got.last(), Some(&204));
}

#[test]
fn compaction_keeps_newest_value() {
    let dir = TempDir::new("compact-dedupe");
    let mut db = Db::open(dir.path(), Config::default()).unwrap();

    db.put(1, 100, b"", b"old").unwrap();
    db.flush().unwrap();
    db.put(1, 100, b"", b"new").unwrap();
    db.flush().unwrap();

    db.compact().unwrap();
    assert_eq!(db.get(1, 100, b"").unwrap(), Some(b"new".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
}

#[test]
fn compaction_drops_expired_records() {
    let dir = TempDir::new("compact-expired");
    let clock = common::TestClock::new(1_000);
    let config = Config {
        default_ttl_ms: Some(1_000),
        ..Config::default()
    };

    let mut db = Db::open_with_clock(dir.path(), config, clock.handle()).unwrap();
    db.put(1, 900, b"", b"stale").unwrap(); // 1900 过期
    db.put(1, 1_500, b"", b"alive").unwrap(); // 2500 过期
    db.flush().unwrap();
    assert_eq!(db.level_counts(), (1, 0));

    clock.set(2_000);
    let report = db.compact().unwrap();
    assert_eq!(report.inputs, 1);
    assert_eq!(report.expired_dropped, 1);
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
}

#[test]
fn compaction_splits_output_by_target_size() {
    let dir = TempDir::new("compact-split");
    let mut db = Db::open(dir.path(), with_target(150)).unwrap();

    db.put(1, 1, b"", b"0123456789").unwrap();
    for ts in 0..20i64 {
        db.put(1, ts * 10, b"", b"0123456789").unwrap();
    }
    db.flush().unwrap();

    let report = db.compact().unwrap();
    assert!(report.outputs > 1, "应切分成多个 L1 文件: {report:?}");
    assert_eq!(db.level_counts(), (0, report.outputs));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 21);
}

#[test]
fn l0_is_searched_before_l1_and_survives_reopen() {
    let dir = TempDir::new("compact-overlay");
    {
        let mut db = Db::open(dir.path(), Config::default()).unwrap();
        db.put(1, 100, b"", b"l1").unwrap();
        db.flush().unwrap();
        db.compact().unwrap();
        assert_eq!(db.level_counts(), (0, 1));

        db.put(1, 100, b"", b"l0").unwrap();
        db.flush().unwrap();
        assert_eq!(db.get(1, 100, b"").unwrap(), Some(b"l0".to_vec()));
        assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
    }

    let db = Db::open(dir.path(), Config::default()).unwrap();
    assert_eq!(db.level_counts(), (1, 1));
    assert_eq!(db.get(1, 100, b"").unwrap(), Some(b"l0".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
}
