# AIBOX-TSDS

面向边缘 AI 盒子（ARM64 / eMMC / 内存受限）的轻量级时序数据存储引擎，Rust + LSM-Tree 实现，
最终编译为 `libtsds.so` 并通过 C ABI 供 C++ 算法插件 FFI 调用。

- **存什么**：录像索引与元数据、告警事件、AI 检测结果等结构化时序元数据。
- **不存什么**：视频文件本体、图片/抓拍等大对象（TSDS 只存路径引用）。

设计依据：[`docs/AIBOX-TSDS.md`](docs/AIBOX-TSDS.md)（接口以其中 3.2 节 `tsds.h` 为唯一真源）。

## 当前进度

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| 1 | 引擎核心：脚手架 → 内存表 → WAL → SSTable → 范围查询 → TTL/淘汰 → Compaction → 批量写 → FFI | 🚧 第 1 步（脚手架）已完成 |
| 2 | 验证与对比：异常测试矩阵、SQLite 基线对比、《基线对比报告.md》 | ⬜ 未开始 |
| 3 | 交叉编译 aarch64 `libtsds.so` + C demo 验证 | ⬜ 未开始 |
| 4 | 外部集成（C++ 插件接入告警事件场景 A） | ⬜ 不在本仓库 |

## 构建与测试

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release     # 产出 target/release/tsds.dll（Linux: libtsds.so）
```

## 目录职责

```text
src/lib.rs      门面：模块声明、重导出、version()、全局 lint 策略
src/error.rs    统一错误类型与 Result 别名（第 9 步映射为 C 错误码）
tests/          集成测试（按路线图步骤逐个增加）
docs/           设计文档
include/tsds.h  C ABI 头文件（阶段 1 第 9 步产出）
```

## 约定

- 内核前 3 步 `#![forbid(unsafe_code)]`；第 4 步（mmap）与第 9 步（FFI）起降级为 `deny` + 局部 `allow`。
- 不设 `panic = "abort"`：第 9 步需用 `catch_unwind` 把 panic 转成 C 错误码，避免拖垮宿主进程。
- `Cargo.lock` 入库，保证边缘设备构建可复现。
