//! 第 1 步冒烟测试：证明包可编译、`[lib] name = "tsds"` 生效、公共 API 可达。

#[test]
fn exposes_package_version() {
    assert_eq!(tsds::version(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn error_implements_display_and_source() {
    use std::error::Error;

    let err: tsds::error::TsdsError = std::io::Error::other("disk on fire").into();
    assert!(err.to_string().contains("disk on fire"));
    assert!(err.source().is_some(), "Io 变体应能取到 source");
}
