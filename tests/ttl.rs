//! 第 6 步：TTL 过期与容量淘汰。

mod common;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use common::TempDir;
use tsds::config::Config;
use tsds::db::Db;

struct TestClock {
    cell: Arc<AtomicI64>,
}

impl TestClock {
    fn new(start: i64) -> Self {
        Self {
            cell: Arc::new(AtomicI64::new(start)),
        }
    }

    fn set(&self, value: i64) {
        self.cell.store(value, Ordering::Relaxed);
    }

    fn handle(&self) -> Box<dyn Fn() -> i64 + Send + Sync> {
        let cell = Arc::clone(&self.cell);
        Box::new(move || cell.load(Ordering::Relaxed))
    }
}

#[test]
fn expired_records_are_hidden_from_get_and_range() {
    let dir = TempDir::new("ttl-hidden");
    let clock = TestClock::new(2_000);
    let config = Config {
        default_ttl_ms: Some(1_000),
        ..Config::default()
    };

    let mut db = Db::open_with_clock(dir.path(), config, clock.handle()).unwrap();
    db.put(1, 1_500, b"", b"alive").unwrap();
    db.put(1, 900, b"", b"stale").unwrap();

    assert_eq!(db.get(1, 1_500, b"").unwrap(), Some(b"alive".to_vec()));
    assert_eq!(db.get(1, 900, b"").unwrap(), None);

    let live: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(live, vec![1_500]);

    clock.set(2_600);
    assert_eq!(db.get(1, 1_500, b"").unwrap(), None);
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 0);
}

#[test]
fn flush_drops_expired_records() {
    let dir = TempDir::new("ttl-flush");
    let clock = TestClock::new(2_000);
    let config = Config {
        default_ttl_ms: Some(1_000),
        ..Config::default()
    };

    {
        let mut db = Db::open_with_clock(dir.path(), config, clock.handle()).unwrap();
        db.put(1, 1_500, b"", b"alive").unwrap();
        db.put(1, 900, b"", b"stale").unwrap();
        let meta = db.flush().unwrap().expect("应产出 SST");
        assert_eq!(meta.entry_count, 1, "过期记录不应写入 SST");
    }

    let db = Db::open_with_clock(dir.path(), config, clock.handle()).unwrap();
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
}

#[test]
fn purge_expired_rewrites_sst() {
    let dir = TempDir::new("ttl-purge");
    let clock = TestClock::new(2_000);
    let config = Config {
        default_ttl_ms: Some(1_000),
        ..Config::default()
    };

    let mut db = Db::open_with_clock(dir.path(), config, clock.handle()).unwrap();
    db.put(1, 1_800, b"", b"drop").unwrap();
    db.put(1, 1_900, b"", b"keep").unwrap();
    db.flush().unwrap();
    assert_eq!(db.sstable_count(), 1);

    // 1800 + 1000 = 2800 已过期；1900 + 1000 = 2900 仍存活
    clock.set(2_850);
    assert_eq!(db.purge_expired().unwrap(), 1);
    assert_eq!(db.sstable_count(), 1);

    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got, vec![1_900]);

    drop(db);
    let db = Db::open_with_clock(dir.path(), config, clock.handle()).unwrap();
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
}

#[test]
fn capacity_eviction_drops_oldest_sst() {
    let dir = TempDir::new("ttl-capacity");
    let config = Config {
        sst_block_bytes: 64,
        capacity_bytes: 400,
        ..Config::default()
    };

    let mut db = Db::open(dir.path(), config).unwrap();
    for round in 0..3i64 {
        for ts in 0..5i64 {
            db.put(1, round * 100 + ts, b"", b"0123456789").unwrap();
        }
        db.flush().unwrap();
    }
    assert_eq!(db.sstable_count(), 3);

    let evicted = db.enforce_capacity().unwrap();
    assert!(evicted >= 1, "应至少淘汰一个最旧 SST");
    assert_eq!(db.sstable_count(), 1);

    let got: Vec<i64> = db
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(got, vec![200, 201, 202, 203, 204], "只应保留最新的数据");
}

#[test]
fn without_ttl_nothing_expires() {
    let dir = TempDir::new("ttl-none");
    let clock = TestClock::new(0);
    let mut db = Db::open_with_clock(dir.path(), Config::default(), clock.handle()).unwrap();
    db.put(1, 1_000, b"", b"v").unwrap();

    clock.set(i64::MAX / 2);
    assert_eq!(db.get(1, 1_000, b"").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.range(1, 0, i64::MAX).unwrap().count(), 1);
    assert_eq!(db.purge_expired().unwrap(), 0);
}
