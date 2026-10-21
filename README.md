# AIBOX-TSDS

面向边缘 AI 盒子（ARM64 / eMMC / 内存受限）的轻量级时序数据存储引擎，Rust + LSM-Tree 实现，
最终编译为 `libtsds.so` 并通过 C ABI 供 C++ 算法插件 FFI 调用。

- **存什么**：录像索引与元数据、告警事件、AI 检测结果等结构化时序元数据。
- **不存什么**：视频文件本体、图片/抓拍等大对象（TSDS 只存路径引用）。

设计依据：[`docs/AIBOX-TSDS-v2.0.md`](docs/AIBOX-TSDS-v2.0.md)（落地版，含实现细节与阶段 2/3 规划）；
原始设计归档于 [`docs/AIBOX-TSDS.md`](docs/AIBOX-TSDS.md)（v1.0，接口以其中 3.2 节 `tsds.h` 为唯一真源）。

## 当前进度

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| 1 | 引擎核心：脚手架 → 内存表 → WAL → SSTable → 范围查询 → TTL/淘汰 → Compaction → 批量写 → FFI | ✅ 已完成（9 步） |
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

C ABI 冒烟测试（需要 Linux/WSL2 + gcc）：

```bash
cargo build --release
bash c_demo/run.sh
```

## 目录职责

```text
src/lib.rs          门面：模块声明、version()、全局 lint 策略
src/error.rs        统一错误类型；ffi 层映射为 C 错误码
src/config.rs       内存预算拆分、TTL、容量、WAL 段与 SST 块大小、合并参数
src/encoding.rs     内部键：channel(4B BE) ‖ timestamp(8B BE, 符号位翻转) ‖ user_key
src/memtable/       有序内存表（BTreeMap）+ 字节占用估算
src/wal/            WAL：CRC32C 记录格式、追加写 + fsync、段轮转、崩溃重放
src/sstable/        块/索引/文件尾的 SST 格式，构建器与读取器
src/manifest.rs     MANIFEST：按层记录有效 SST，原子更新；IdAllocator
src/iterator.rs     Cursor 抽象 + BinaryHeap 多路归并（同键取最新）
src/retention.rs    TTL 判定与存活下界
src/compaction.rs   L0→L1 合并内核 + 令牌桶写限速
src/db.rs           引擎门面：put / batch_put / get / range / scan / flush / compact
src/ffi/            C ABI：句柄、panic 拦截、错误码映射
include/tsds.h      C ABI 头文件（对外契约，勿改签名）
c_demo/             C 侧冒烟测试（阶段 3 在 Linux 下运行）
tests/              集成测试，按功能分文件
docs/               设计文档
```

## 语义约定

- **写入持久性**：`tsds_put` / `tsds_batch_put` 返回成功 = 已写入 WAL 并 fsync；未返回的写入可能丢失。
- **批量语义**：`tsds_batch_put` 把多条记录编码进**一条** WAL 物理记录，一次追加、一次刷盘；返回 `0` 全成功、`>0` 前 N 条成功、`<0` 整体失败。
- **键语义**：同一 `(channel, timestamp, user_key)` 后写覆盖；`user_key` 不参与查询结果输出。
- **区间**：查询为 `[start_ms, end_ms)`，左闭右开。
- **生命周期**：`Config::default_ttl_ms` 决定过期（`ts + ttl <= now` 即过期）；查询自动隐藏过期数据，`flush` 不写过期记录，`purge_expired` 重写含过期记录的 SST。
- **容量**：超过 `capacity_bytes` 时按"最旧优先"删除 SST（至少保留一个）。
- **合并**：`flush` 后 L0 文件数达到 `compaction_trigger_l0` 自动触发后台合并；后台只做 IO，提交在前台，写入永不等待合并。`compaction_bytes_per_sec` 限速（0 = 不限速）。
- **错误码**：`0` 成功、`-1` 参数非法、`-2` IO、`-3` 数据损坏、`-4` 内部错误。

## 约定

- 内核 `#![deny(unsafe_code)]`，只有 `src/ffi` 模块 `#![allow(unsafe_code)]`。
- 不设 `panic = "abort"`：FFI 边界用 `catch_unwind` 把 panic 转成错误码，避免拖垮宿主进程。
- `Cargo.lock` 入库，保证边缘设备构建可复现。
