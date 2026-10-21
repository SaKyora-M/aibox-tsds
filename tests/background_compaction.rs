//! 第 7b 步：后台合并、写限速。

mod common;

use std::time::Instant;

use common::TempDir;
use tsds::config::Config;
use tsds::db::Db;

fn config() -> Config {
    Config {
        compaction_trigger_l0: 2,
        ..Config::default()
    }
}

#[test]
fn background_compaction_moves_l0_to_l1() {
    let dir = TempDir::new("bg-compact");
    let mut db = Db::open(dir.path(), config()).unwrap();

    for ts in [10i64, 20, 30] {
        db.put(1, ts, b"", b"a").unwrap();
    }
    db.flush().unwrap();
    assert_eq!(db.level_counts(), (1, 0));

    for ts in [40i64, 50] {
        db.put(1, ts, b"", b"b").unwrap();
    }
    db.flush().unwrap();
    assert!(db.compaction_pending(), "L0 达到阈值应触发后台合并");

    assert!(db.wait_compaction().unwrap());
    assert_eq!(db.level_counts(), (0, 1));

    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got, vec![10, 20, 30, 40, 50]);
}

#[test]
fn writes_are_not_blocked_by_rate_limited_compaction() {
    let dir = TempDir::new("bg-nonblocking");
    let mut cfg = config();
    cfg.compaction_bytes_per_sec = 512;
    let mut db = Db::open(dir.path(), cfg).unwrap();

    for ts in 0..5i64 {
        db.put(1, ts, b"", b"0123456789").unwrap();
    }
    db.flush().unwrap();
    for ts in 5..10i64 {
        db.put(1, ts, b"", b"0123456789").unwrap();
    }
    db.flush().unwrap();
    assert!(db.compaction_pending());

    let start = Instant::now();
    db.put(1, 99, b"", b"v").unwrap();
    assert!(start.elapsed().as_millis() < 500, "写入不应等待合并完成");

    assert!(db.wait_compaction().unwrap());
    assert_eq!(db.level_counts().0, 0);
    assert_eq!(db.get(1, 99, b"").unwrap(), Some(b"v".to_vec()));
}
