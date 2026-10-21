//! 第 5 步：流式归并查询。

mod common;

use common::TempDir;
use tsds::config::Config;
use tsds::db::Db;

fn open(tag: &str) -> (TempDir, Db) {
    let dir = TempDir::new(tag);
    let db = Db::open(dir.path(), Config::default()).unwrap();
    (dir, db)
}

#[test]
fn merges_memtable_and_sst_in_order() {
    let (_dir, mut d) = open("range-merge");
    for ts in [10i64, 30, 50] {
        d.put(1, ts, b"", format!("sst-{ts}").as_bytes()).unwrap();
    }
    d.flush().unwrap();
    for ts in [20i64, 40] {
        d.put(1, ts, b"", format!("mem-{ts}").as_bytes()).unwrap();
    }

    let got: Vec<(i64, String)> = d
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| {
            let (ts, value) = item.unwrap();
            (ts, String::from_utf8(value).unwrap())
        })
        .collect();

    assert_eq!(
        got,
        vec![
            (10, "sst-10".to_string()),
            (20, "mem-20".to_string()),
            (30, "sst-30".to_string()),
            (40, "mem-40".to_string()),
            (50, "sst-50".to_string()),
        ]
    );
}

#[test]
fn duplicate_key_keeps_newest_value() {
    let (_dir, mut d) = open("range-dedupe");
    d.put(1, 100, b"", b"old").unwrap();
    d.flush().unwrap();
    d.put(1, 100, b"", b"new").unwrap();

    let got: Vec<(i64, String)> = d
        .range(1, 0, i64::MAX)
        .unwrap()
        .map(|item| {
            let (ts, value) = item.unwrap();
            (ts, String::from_utf8(value).unwrap())
        })
        .collect();

    assert_eq!(got, vec![(100, "new".to_string())]);
}

#[test]
fn take_stops_early() {
    let (_dir, mut d) = open("range-take");
    for ts in 0..10i64 {
        d.put(1, ts, b"", b"v").unwrap();
    }

    let first: Vec<i64> = d
        .range(1, 0, i64::MAX)
        .unwrap()
        .take(3)
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(first, vec![0, 1, 2]);
}

#[test]
fn empty_range_yields_nothing() {
    let (_dir, mut d) = open("range-empty");
    d.put(1, 100, b"", b"v").unwrap();
    assert_eq!(d.range(1, 100, 100).unwrap().count(), 0);
    assert_eq!(d.range(1, 0, 100).unwrap().count(), 0);
}
