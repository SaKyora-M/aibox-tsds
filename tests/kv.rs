//! 最小 KV 通路（含 WAL 与 SST 读路径）。

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
fn write_then_read_back() {
    let (_dir, mut d) = open("kv-write");
    d.put(3, 1_700_000_000_000, b"alarm:1", b"person").unwrap();
    assert_eq!(
        d.get(3, 1_700_000_000_000, b"alarm:1").unwrap(),
        Some(b"person".to_vec())
    );
    assert_eq!(d.get(4, 1_700_000_000_000, b"alarm:1").unwrap(), None);
    assert_eq!(d.get(3, 1_700_000_000_001, b"alarm:1").unwrap(), None);
}

#[test]
fn same_key_overwrites() {
    let (_dir, mut d) = open("kv-overwrite");
    d.put(1, 10, b"k", b"v1").unwrap();
    d.put(1, 10, b"k", b"v2").unwrap();
    assert_eq!(d.get(1, 10, b"k").unwrap(), Some(b"v2".to_vec()));
    assert_eq!(d.range(1, 0, 100).unwrap().count(), 1);
}

#[test]
fn range_is_ordered_and_left_closed_right_open() {
    let (_dir, mut d) = open("kv-range");
    for ts in [200i64, 100, 150] {
        d.put(2, ts, b"", format!("v{ts}").as_bytes()).unwrap();
    }

    let got: Vec<(i64, String)> = d
        .range(2, 100, 200)
        .unwrap()
        .map(|item| {
            let (ts, value) = item.unwrap();
            (ts, String::from_utf8(value).unwrap())
        })
        .collect();

    assert_eq!(
        got,
        vec![(100, "v100".to_string()), (150, "v150".to_string())]
    );
}
