# 《AIBOX-TSDS 轻量级时序数据存储引擎技术方案书》

| 项 | 内容 |
| --- | --- |
| 版本 | **v2.0（落地版）** |
| 日期 | 2026-10-21 |
| 状态 | 阶段 1（引擎核心）已完成并验收；本版在 v1.0 设计基础上按实际实现修订 |
| 依据 | v1.0《AIBOX-TSDS 技术方案书》+ 阶段 1 实际代码与测试 |
| 代码位置 | `D:\CodeProject\RustProject\aibox-tsds`（提交 `d172778`） |

**版本变更说明**

- v1.0：项目立项设计（架构、接口、能力设想）。
- **v2.0：按实际实现回写**。接口签名与 v1.0 完全一致（未改动 `tsds.h`）；对 v1.0 中未定义或无法按原设想实现的点，在本版逐条说明原因与替代方案（见 §2、§8.3）。新增存储格式、提交顺序与崩溃点分析、配置全表、阶段 2/3 详细规划。

---

## 一、项目简介

AIBOX-TSDS 是基于 Rust + LSM-Tree 的轻量级时序数据存储引擎，运行于边缘 AI 盒子（ARM64，eMMC/SSD，内存受限），存储录像索引、告警事件、AI 检测结果等**结构化时序元数据**。引擎编译为 `libtsds.so`，导出 C ABI，供现有 C++ 算法插件经 FFI 调用，不改动现有软件系统架构。

**职责边界**

| ✅ 存什么 | ❌ 不存什么 |
| --- | --- |
| 录像文件索引与元数据（路径、起止时间、通道） | 视频文件本体（仍走文件系统） |
| 告警事件记录（时间、通道、类型、等级） | 告警关联图片/抓拍等大对象（仅存路径） |
| AI 检测结果等结构化时序元数据 | 任何非结构化二进制大对象 |

**技术栈实测口径**

| 项 | 值 |
| --- | --- |
| 语言 / 工具链 | Rust 1.99.0 stable，edition 2021 |
| 第三方运行时依赖 | **0 个**（CRC32C 自行实现，无 `serde`/`tokio`/`crc`） |
| 交付形态 | `cdylib`（`libtsds.so` / `tsds.dll`）+ `staticlib` + `rlib`（测试用） |
| 代码规模 | 源码 3,583 行（含内联单测），测试 42 条 × 11 文件 |
| 默认内存上限 | 64 MB（`tsds_open` 传 0 时启用） |

---

## 二、v2.0 相对 v1.0 的设计修订

v1.0 是设计意图，落地时遇到四处"无法按原样实现"或"原设计未定义"的点。修订如下，**接口签名一律未改**。

| # | 项 | v1.0 设计 | v2.0 实现 | 原因 |
| --- | --- | --- | --- | --- |
| 1 | 同一 `(通道, 时间戳)` 多记录 | 未定义 | 内部键扩展为 `channel ‖ timestamp ‖ user_key` 三级排序，同三级键**后写覆盖** | 同一毫秒可能有多条不同类型告警；`tsds_put` 已带 `key`，用它做第三级排序天然幂等 |
| 2 | TTL 载体 | "记录可携带生存时间" | 配置级 TTL：`过期时刻 = timestamp + default_ttl_ms` | `tsds.h` 的 `tsds_put` 没有 TTL 参数，不得改签名；记录自带时间戳，派生 TTL 无需额外存储 |
| 3 | 分级保留（按等级/类型） | 列为"扩展"策略 | **未实现**，`EvictionPolicy::TieredRetention` 仅保留枚举 | 同上：C ABI 没有"等级"字段，实现等于自造接口；等场景 A 的 value schema 定稿后再做 |
| 4 | 容量淘汰 | "按时间顺序淘汰最旧记录" | 按**最旧 SST 文件**删除（近似按时间） | L0 文件按 flush 顺序产生，时间上近似单调；严格按记录时间淘汰需要 compaction 输出带时间区间（列为阶段 3 之后的改进） |
| 5 | 内存峰值 | "内存峰值可配置，默认 64MB" | 内存上限**拆成四份预算**并驱动 flush/合并，但不做全局强制回收 | 无 GC 的 LSM 无法"强制"回收；通过"MemTable 阈值触发 flush + 合并限速"间接约束峰值 |
| 6 | 在线快照（§2.4） | 运行中导出一致性快照 | **未实现**（崩溃恢复已实现） | 快照需要对全部 SST + MemTable 做一致性复制点，工作量与 compaction 相当；已列入阶段 3 之后 |

---

## 三、总体架构（实际）

### 3.1 模块划分与依赖方向

```text
                        ┌──────────────┐
                        │  ffi（C ABI） │  handle.rs / api.rs
                        └──────┬───────┘
                               │ 只做指针/句柄/错误码/panic 归一化
                        ┌──────▼───────┐
                        │      db      │  读写路径 + 实例生命周期
                        └──┬───┬───┬───┘
          ┌────────────────┘   │   └────────────────┐
   db/compaction        db/maintenance          iterator
   （计划/后台/提交）     （TTL/容量）         （Cursor + 归并）
          │                  │                    │
   ┌──────┴───────┬──────────┴───────┬────────────┴────┐
   │  sstable     │      wal         │    memtable     │
   └──────┬───────┴──────────┬───────┴────────┬────────┘
          │                  │                │
      manifest          retention         encoding
          └──────────┬───────┴────────────────┘
                     │
        checksum / platform / config / error
```

**依赖规则**（阶段 1 自检通过：0 循环依赖）

- 上层只依赖下层，`encoding` / `checksum` / `platform` / `config` / `error` 是叶子；
- `db` 是唯一的编排者，`ffi` 不含任何存储逻辑；
- `iterator` 只依赖"能按序吐记录"的 `Cursor` 抽象，不依赖 WAL。

### 3.2 模块清单

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/db.rs` | 370 | 实例生命周期、`put`/`batch_put`/`get`/`range`/`scan`/`flush` |
| `src/db/compaction.rs` | 244 | 合并计划（选输入）、后台执行、前台提交 |
| `src/db/maintenance.rs` | 123 | TTL 物理清理、容量淘汰、SST 重写 |
| `src/ffi/api.rs` | 285 | 7 个 `extern "C"` 导出函数 |
| `src/ffi/handle.rs` | 94 | 句柄、`#[repr(C)]` 条目、错误码、panic 拦截 |
| `src/sstable/reader.rs` | 268 | 块索引定位、块内扫描、范围迭代 |
| `src/wal/record.rs` | 250 | WAL 记录格式（Put / Batch）编解码 + CRC |
| `src/manifest.rs` | 243 | 按层记录 SST 集合，原子更新；线程安全 id 分配 |
| `src/compaction.rs` | 162 | 合并内核：归并、去重、丢过期、分片、令牌桶限速 |
| `src/iterator.rs` | 218 | `Cursor` 抽象 + `BinaryHeap` 多路归并 + 同键去重 |
| `src/encoding.rs` | 198 | 内部键编码与扫描边界 |
| `src/config.rs` | 168 | 配置与内存预算拆分 |
| `src/memtable/mod.rs` | 162 | 有序内存表（`BTreeMap`）与字节占用估算 |
| `src/sstable/builder.rs` | 159 | SST 构建（块、索引、文件尾、fsync） |
| `src/wal/writer.rs` | 151 | 追加写、fsync、段轮转、段清理 |
| `src/checksum.rs` | 87 | 表驱动 CRC32C + LevelDB 风格掩码 |
| `src/platform.rs` | 41 | 目录 fsync、跨平台定位读 |
| 其余（`lib`/`error`/`retention`/`sstable/block`/`sstable/mod`/`ffi/mod`） | 8–74 | 门面、错误、TTL 判定、块编解码、SST 索引编码 |

### 3.3 四条关键路径

**写路径**（`tsds_put`）

```text
ffi 层校验指针 → Arc<Mutex<Db>> 加锁 → 构造内部键 → WAL 编码一条记录
→ write_all + sync_data（fsync）→ 成功后写入 MemTable → 返回 0
```

返回 `0` 的语义是**已 fsync**；失败或未返回的写入可能丢失。

**批量写路径**（`tsds_batch_put`）

```text
校验每条 → 打包成一条 KIND_BATCH 物理记录（count + 每条 key_len/key/value_len/value）
→ 一次 write_all + 一次 sync_data → 逐条进 MemTable → 返回 0 / 前 N 条成功
```

一次系统调用、一次 fsync，这是"高频事件"场景的主要优化点。

**读路径**

```text
get:   MemTable → L0（新到旧）→ L1，命中即返回
range: MemTable 游标 + 各 SST 游标 → BinaryHeap 多路归并 → 同键取最新 → 流式产出
scan:  同上，但限制本页条数并支持 `after` 游标续扫（FFI 迭代器用）
```

**恢复路径**

```text
open → 列出 wal/*.log 按段号升序 → 逐段解码（Put/Batch）
→ 校验 CRC，末段遇到撕裂/校验失败即停止并记录有效长度
→ 截断末段到有效长度（否则后续追加会落在垃圾之后、下次重放提前中断）
→ 打开 MANIFEST，按层加载 SST 读取器
```

**维护路径**

```text
flush:   MemTable（跳过已过期）→ 写 L0 SST（fsync）→ MANIFEST 登记 → 清 WAL 段 → 清 MemTable
compact: 全部 L0 + 键区间重叠的 L1 → 归并去重丢过期 → 按 8MB 切片写 L1 → 前台提交
         （后台线程只读输入、写输出；MANIFEST 与内存视图只在前台改）
```

---

## 四、存储格式（实际落地）

### 4.1 目录布局

```text
<path>/
├── MANIFEST                 # 有效 SST 清单（层级 + id），原子更新
├── wal/
│   └── 000001.log           # 段号 6 位；默认单段 4MB，超限轮转
└── sst/
    ├── L0/
    │   └── 000001.sst
    └── L1/
        └── 000002.sst
```

> v1.0 设计中的 `tsds.conf` 与 `snapshot/` 目录本版未创建：配置当前由 `Config` 结构体在进程内提供（`tsds_open` 只有 `mem_limit_mb` 一个入参），快照能力未实现（见 §2.6）。

### 4.2 内部键

```text
channel(4B, 大端) ‖ timestamp(8B, 大端，最高位翻转) ‖ user_key(变长，可空)
└────────── 12 字节定长头部 ──────────┘
```

- **大端**：字节序比较 == 数值比较，使 BTreeMap / SST 块内排序 / 归并比较统一为一次 `slice::cmp`；
- **最高位翻转**（异或 `1<<63`）：`i64` 直接大端编码时负数排到正数之后，翻转后全范围有序；
- **channel 前缀**：按通道查询退化为一段连续区间（一次 seek + 顺序扫描），而不是全表过滤；
- **user_key 末位**：承载"覆盖身份"与"同刻多记录"的区分，且**不进查询结果**（`tsds_query` 只返回 timestamp + value）。

扫描区间 `[start_ms, end_ms)` 的下界取 `channel‖ts(start)‖""`（闭），上界取 `channel‖ts(end)‖""`（开）——上界恰好是"ts == end 的所有键"中最小的一个，因此无需后继键、也不会溢出。

### 4.3 WAL 记录

```text
crc32c(4B LE, 掩码) ‖ len(4B LE) ‖ kind(1B) ‖ payload(len B)     头部 9 字节
CRC 覆盖 kind ‖ payload
```

- `kind = 1`（Put）：`payload = key_len(4B LE) ‖ key ‖ value`（value 取剩余长度）
- `kind = 2`（Batch）：`payload = count(4B LE) ‖ [key_len ‖ key ‖ value_len ‖ value] * count`

CRC32C 自行实现（反射多项式 `0x82F63B78`，表驱动），并施加 LevelDB 风格掩码（`rotate_right(15) + 0xa282ead8`）以降低"尾部补零不改变 CRC"的漏检。payload 上限 16MB，用于识别损坏的长度字段。

### 4.4 SST 文件

```text
[数据块 0][数据块 1]...[索引块][文件尾 16B]
数据块条目：key_len(4B LE) ‖ key ‖ value_len(4B LE) ‖ value
索引块条目：key_len(4B LE) ‖ 块内最后键 ‖ offset(8B LE) ‖ len(4B LE)
文件尾    ：index_offset(8B) ‖ index_len(4B) ‖ magic "TSDS"(4B)
```

默认块 4KB。点查流程：索引二分定位块 → 读块 → 块内二分 → 比对键。范围查询：索引定位起始块 → 顺序读块 → 块内顺序扫描。

> 本版**不含**前缀压缩、重启点、布隆过滤器、块缓存——读放大会在阶段 2 用数据量化后再决定是否补（见 §8.3）。

### 4.5 MANIFEST

```text
tsds-manifest-v2
000001 0
000002 1
```

每行 `{id:06} {level}`。写入流程：写 `MANIFEST.tmp` → `sync_all` → `rename` 覆盖 → 目录 fsync。兼容 `tsds-manifest-v1`（无 level 列的旧格式自动视为 L0）。`id` 由 `Arc<AtomicU64>` 分配，与后台合并线程共享同一 id 空间；分配时跳过已存在的孤儿文件。

### 4.6 提交顺序与崩溃点分析

**flush 的提交顺序**：`写 SST + fsync` → `MANIFEST 登记` → `WAL 轮转并删除旧段` → `清空 MemTable`。

| 崩溃点 | 磁盘状态 | 恢复行为 |
| --- | --- | --- |
| SST 写到一半 | 孤儿 `.sst`（未登记） | 重开时不加载；下次分配 id 时跳过 |
| SST 完整、MANIFEST 未写 | 孤儿 `.sst` | 同上 |
| MANIFEST 已写、WAL 未删 | 数据在 SST 且 WAL 仍有副本 | 重放产生同键写入，**幂等**，不丢不重 |
| WAL 已删、MemTable 未清 | 数据只在 SST | 正常，MemTable 清空即可 |
| WAL 尾部撕裂 | 末尾半条记录 | 重放停止并截断到有效长度 |

**结论**：任意时刻掉电，已返回成功的写入不丢；最坏情况是重复重放（幂等）。这是把"MANIFEST 提交"放在"删 WAL"之前的原因。

---

## 五、核心能力实现情况

| 能力（v1.0 章节） | 实现状态 | 落地方式 | 验证测试 |
| --- | --- | --- | --- |
| 时间范围查询（§2.1） | ✅ | 三级有序键 + 游标归并 + 左闭右开区间 | `encoding::scan_bounds_*`、`range::*`、`ffi_iterator_pages_*` |
| 流式迭代（不受内存限制） | ✅ | 内核流式归并；C 侧每页 256 条续扫 | `range::take_stops_early`、`ffi_iterator_pages_through_large_result` |
| TTL 自动过期（§2.2） | ✅（配置级） | 查询起点下推 + flush 跳过 + `purge_expired` 重写 | `ttl::*`（5 条） |
| 容量淘汰（删最旧） | ✅（近似） | 按最旧 SST 删除，至少保留一个 | `ttl::capacity_eviction_drops_oldest_sst` |
| 分级保留 | ⬜ | 见 §2.3 | — |
| 批量写入（§2.3） | ✅ | 一条 Batch 物理记录、一次 fsync | `batch::*`（5 条） |
| 崩溃恢复（§2.4） | ✅ | 段重放 + CRC + 末段截断 | `wal_recovery::*`（4 条） |
| 在线快照（§2.4） | ⬜ | 见 §2.6 | — |
| C ABI 全部接口（§3.2） | ✅ | 7 个导出函数，句柄 + panic 拦截 + 错误码 | `ffi::*`（6 条）+ 导出符号检查 |
| 内存受限（§1） | ✅（预算驱动） | 四份预算 + flush 阈值 + 合并限速 | `config::budget_*` |
| L0→L1 合并 + 后台 + 限速（§2.2 延伸） | ✅ | 前台提交、后台 IO、令牌桶 | `compaction::*`(5) + `background_compaction::*`(2) |

---

## 六、配置与默认值

| 配置项 | 默认值 | 说明 |
| --- | --- | --- |
| `mem_limit_mb` | 64（下限 16） | `tsds_open` 入参，0 表示默认 |
| 预算：MemTable / 落盘中 / 块缓存 / 余量 | 16 / 16 / 24 / 8 MB | 25% / 25% / 37.5% / 12.5%，四项之和恒等于上限 |
| `wal_segment_bytes` | 4 MiB | 单段 WAL 上限，超限轮转 |
| `sst_block_bytes` | 4096 | SST 数据块目标大小 |
| `capacity_bytes` | 512 MiB | 存储上限，超限按最旧 SST 淘汰 |
| `l1_target_bytes` | 8 MiB | 合并输出分片目标 |
| `compaction_trigger_l0` | 4 | L0 文件数达到即触发后台合并 |
| `compaction_bytes_per_sec` | 8 MiB/s | 合并写限速，0 = 不限速 |
| `background_compaction` | true | 是否启用后台合并 |
| `default_ttl_ms` | None | 记录过期时间（派生自记录时间戳） |
| `MAX_VALUE_LEN` | 1 MiB | 单条 value 硬上限（方案建议 ≤ 4KB） |

---

## 七、C ABI

接口与 v1.0 §3.2 **完全一致**（`include/tsds.h`，7 个导出函数 + `tsds_item` 结构体），未做任何签名修改。语义补充如下：

| 项 | 语义 |
| --- | --- |
| 返回值 | `0` 成功；`-1` 参数非法；`-2` IO；`-3` 数据损坏；`-4` 内部错误（含 panic、句柄问题） |
| `tsds_batch_put` | `0` 全成功；`>0` 前 N 条成功；`<0` 整体失败（与其余接口不同，调用方注意） |
| 迭代器值指针 | 在下一次 `tsds_iter_next` 或 `tsds_iter_free` 前有效，调用方不得释放 |
| 迭代器与关闭 | `tsds_close` 后迭代器仍可读完（内部持有实例引用），但仍须调 `tsds_iter_free` |
| NULL 容忍 | `tsds_iter_free(NULL)`、`tsds_close(NULL)` 安全；其余 NULL 句柄返回错误码 |
| 线程安全 | 句柄内部互斥，多线程并发调用安全（串行执行） |
| panic | 全部拦截在边界内，不会 abort 宿主进程 |

**内存安全设计**：`ffi` 层用 `Arc<Mutex<Db>>` 持有实例；迭代器持有同一 `Arc` 的克隆并在每次续扫时重新加锁，因此既不会与写入形成 `&self`/`&mut self` 别名，也不需要跨 FFI 转换生命周期。

---

## 八、阶段 1 完成情况

### 8.1 九步交付与验收

| 步骤 | 产出 | 关键验收 |
| --- | --- | --- |
| 1 脚手架 | 工程、lint 策略、`crate-type` | `tsds.dll`/`libtsds.so` 产物名正确 |
| 2 内存表 | `encoding`、`config`、`memtable`、`db`（内存版） | 键排序、区间边界、覆盖写内存回收 |
| 3 WAL | `checksum`、`wal/{record,writer}`、`platform` | 重放、撕裂写截断、CRC 损坏丢弃尾条、段轮转 |
| 4 SSTable | `sstable/{block,builder,reader}`、`manifest`、`db::flush` | 多层块往返、MANIFEST 原子更新、WAL 清理 |
| 5 流式查询 | `iterator`、`db::range` | 多路归并、同键取最新、`take` 提前终止 |
| 6 TTL/淘汰 | `retention`、`db::purge_expired/enforce_capacity` | 过期隐藏、flush 跳过、重写 SST、容量淘汰 |
| 7 Compaction | `compaction`、`db/compaction` | L0→L1、去重、丢过期、分片、后台+限速 |
| 8 批量写 | `wal::put_batch`、`Db::batch_put` | **一条**物理记录 + 一次 fsync |
| 9 FFI | `include/tsds.h`、`ffi/{handle,api}` | 7 个导出符号、NULL 安全、panic 拦截、关闭后迭代 |

### 8.2 质量数据（实测）

| 项 | 结果 |
| --- | --- |
| `cargo test` | **81 passed / 0 failed**（单元 39 + 集成 42） |
| `cargo clippy --all-targets -- -D warnings` | 0 warning |
| `cargo fmt --check` | 通过 |
| `cargo build --release` | 成功，`tsds.dll` ≈ 246 KB |
| 导出符号 | 7/7 全部存在（`tsds_open/put/query/iter_next/iter_free/batch_put/close`） |
| 架构自检 | 26 文件，0 循环依赖，0 超大模块（最大 370 行） |
| 运行时依赖 | 0 |
| unsafe 范围 | 仅 `src/ffi`（其余 `#![deny(unsafe_code)]`） |

### 8.3 已知限制（诚实清单）

| # | 限制 | 影响 | 计划 |
| --- | --- | --- | --- |
| 1 | 无删除接口（只有覆盖写） | 场景 B（录像覆盖删索引）无法表达 | 阶段 4 前补 `delete` + 墓碑 |
| 2 | 无布隆过滤器 / 块缓存 | 点查与冷读放大大 | 阶段 2 量化后决定 |
| 3 | 合并仅 L0→L1 单层 | 数据量增大后 L1 触发全量重写，写放大上升 | 阶段 3 之后加 L2 |
| 4 | 容量淘汰按最旧 SST 近似 | 可能先删掉"写入早但时间戳新"的通道数据 | 阶段 3 之后改为按 SST 时间区间淘汰 |
| 5 | `purge_expired` 全表重写 | 清理成本 O(数据量) | 与 compaction 合并 |
| 6 | 内存上限非强制 | 极端场景可超预算（无全局回收器） | 阶段 2 压力测试确认实际峰值 |
| 7 | 无在线快照 | 备份/迁移需停机 | 阶段 3 之后 |
| 8 | **C demo 未在 Linux 实跑** | 头文件与 `.so` 的 C 侧链接未验证（本机无 gcc、WSL 无发行版） | 阶段 3 首项 |
| 9 | **未交叉编译 aarch64** | ARM64 部署未验证 | 阶段 3 |
| 10 | 性能数字全未测量 | 无法对 v1.0 "每秒数千次写入"作结论 | 阶段 2 |
| 11 | Windows 上完成的全部功能验证 | `FlushFileBuffers` 与 `fsync` 语义不同；目录 fsync 在 Windows 为空实现 | 阶段 2 起在 Linux 复验 |

---

## 九、阶段 2 规划：验证与对比

**目标**：把"能跑"变成"可证明"。产出《异常测试报告》与《基线对比报告》，为"是否替换现有 SQLite 索引"提供数据依据。

### 9.1 子阶段拆分

| 子阶段 | 内容 | 产出 |
| --- | --- | --- |
| 2a | 异常测试矩阵 | `tests/fault_injection.rs`、`tests/concurrency.rs` + 报告章节 |
| 2b | 基准测试框架 + SQLite 基线 | `benches/*`、`tests/baseline/*` + 原始数据 CSV |
| 2c | 《基线对比报告.md》 | 结论与建议配置 |

### 9.2 异常测试矩阵（2a）

| 用例 | 注入方式 | 期望 |
| --- | --- | --- |
| 进程崩溃恢复 | 写入 N 条后直接 `std::process::exit`（子进程） | 重开后前 N 条全在 |
| WAL 尾部撕裂（参数化） | 把末条记录截断 1..L 字节 | 前缀数据保留，无 panic |
| WAL 尾部垃圾 | 追加随机字节 | 截断到有效长度，后续写入可重放 |
| WAL 非末段损坏 | 翻转中间段某字节 | 返回 `Corrupted(-3)`，不静默丢数据 |
| SST 数据块损坏 | 翻转块内字节 | `get`/`range` 返回 `Corrupted` |
| SST 文件尾/魔数损坏 | 改 magic / index_offset | `open` 明确报错，不加载 |
| MANIFEST 缺失 / 损坏 | 删除或改首行 magic | 报错或按空清单处理（需定策略） |
| MANIFEST 指向不存在的 SST | 删除一个已登记文件 | 启动报错或跳过并警告（需定策略） |
| 孤儿 SST | 手工放入未登记的 `.sst` | 不加载、不报错、id 分配跳过 |
| 磁盘写满 | Linux 上挂小容量 tmpfs；或调小 `capacity_bytes` + 预填充 | 写入返回 `-2`，已有数据可读 |
| 并发写入 | 8 线程 × 1 万条经 FFI 句柄写入 | 无 panic、无数据丢失、句柄可正常关闭 |
| 写入中并发查询 | 写线程 + 3 读线程（含跨页迭代） | 无 panic、结果单调、迭代不越界 |
| FFI 误用 | NULL 句柄/指针、`start > end`、超限 value、`count = 0`、关闭后 `iter_free` | 返回对应错误码，不崩 |
| 快速开关 | 1000 次 `open`/`close` | 无句柄与 Fd 泄漏（Linux 用 `/proc/self/fd` 计数） |
| 时钟回拨 | 注入时钟使 `now` 后退 | TTL 不误删、不崩溃 |
| 长稳 | 持续写入 1 小时（可缩短） | MANIFEST/SST 数量稳定、无内存增长趋势 |

### 9.3 SQLite 基线（2b）

**公平性配置**（缺一不可，否则结论无效）

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;      -- 与 TSDS 的每次 fsync 对等
PRAGMA cache_size = -16384;     -- 16MB，与 TSDS 预算同量级
PRAGMA wal_autocheckpoint = 1000;
CREATE TABLE events(channel INTEGER, ts INTEGER, key BLOB, value BLOB,
                    PRIMARY KEY(channel, ts, key));
CREATE INDEX idx_channel_ts ON events(channel, ts);
```

**数据集**：告警事件模型，value ≈ 200B，通道 1..16，时间戳单调递增；规模 10 万 / 100 万两档。

**测量项**

| 指标 | 采集方式 |
| --- | --- |
| 单条写入吞吐（条/秒） | 固定 10 万条，`put` 逐条，取 3 轮中位数 |
| 批量写入吞吐 | `batch_put` 100 条/批，同规模 |
| 范围查询延迟 p50/p99/p999 | 1000 次随机时间窗（1 分钟 / 1 小时两档），自建对数直方图 |
| 点查延迟 p99 | 1 万次随机 `(channel, ts, key)` |
| 空间占用 | 目录字节数 / 记录数（含 WAL、SST、MANIFEST） |
| 内存峰值 | Linux `/proc/self/status` 的 `VmHWM` |
| 删最旧延迟 | 触发容量淘汰或 TTL 清理的单次耗时分布 |
| 抖动分布 | 持续写入 10 分钟，每 10 秒窗口的 p99 时间序列 |
| 崩溃恢复时间 | 10 万条写入后重启到可服务 |

**方法学**：criterion 负责微基准；端到端场景自建 harness（避免 criterion 的统计假设与 fsync 场景冲突）。每项 ≥30 次采样，报中位数与 p99，标注标准差。原始数据存 CSV 供复核。

**前置条件（需要你拍板）**

1. **测量环境必须是 Linux**（ext4）。Windows 上的 `FlushFileBuffers` 与 `fsync` 语义不同，数字无参考价值。建议装 WSL2 发行版，或单独一台 Linux 机器。
2. **引入 2 个 dev-dependency**：`criterion`（微基准）与 `rusqlite`（`bundled` 特性，免装系统 SQLite）。二者仅测试期使用，不进交付产物。
3. 图表的生成方式（Python/matplotlib 或纯 Markdown 表格）。

### 9.4 报告结构（2c）

1. 测试环境与可复现步骤（硬件、内核、文件系统、参数、命令）
2. 功能正确性回归（81 条测试 + 异常矩阵结果）
3. 性能对比（每项一张表：TSDS / SQLite，含差值与结论）
4. 抖动与长稳分析（p99 时间序列、毛刺归因：flush / compaction / fsync）
5. 结论与建议：什么负载下用 TSDS、什么负载下建议继续用 SQLite；边缘设备的推荐配置

---

## 十、阶段 3 规划：交叉编译与交付

| 项 | 内容 |
| --- | --- |
| 交叉编译 | `rustup target add aarch64-unknown-linux-gnu` + `aarch64-linux-gnu-gcc` 链接器（或 `cross`/`zig`）；产出 `libtsds.so` |
| 产物校验 | `file` 确认 ELF aarch64；`nm -D` 只应出现 7 个 `tsds_*`；`ldd` 确认 glibc 版本下限；`strip` 后体积 |
| C demo | 在 Linux x86_64 与 aarch64 上分别编译运行 `c_demo/tsds_demo.c`（aarch64 可用 qemu-user 验证） |
| ABI 稳定性 | 导出表只含 7 个符号；Rust 内部符号不外泄；`tsds.h` 与 `.so` 版本绑定说明 |
| 交付包 | `libtsds.so`（aarch64/x86_64）、`libtsds.a`（可选）、`tsds.h`、集成示例、README（含错误码与语义约定）、已知限制 |
| 实盘抽检 | 在目标板 eMMC 上跑一遍写入/查询/掉电恢复，记录 fsync 实测延迟 |
| 风险 | eMMC 写放大（每条 fsync）→ 集成时优先用 `batch_put`；aarch64 上 fsync 延迟可能显著高于 PC；glibc 版本兼容（必要时提供 musl 静态版本） |

---

## 十一、待决策问题

| # | 问题 | 备选 | 影响 |
| --- | --- | --- | --- |
| 1 | 阶段 2 在哪跑基准 | WSL2 / 独立 Linux 机器 | 数字有效性，阶段 2 的硬前置 |
| 2 | 是否引入 `criterion` + `rusqlite` | 引入 / 自写 | 开发速度 vs 依赖洁癖（交付产物不受影响） |
| 3 | 是否需要 `delete` 接口 | 需要（场景 B）/ 暂不需要 | 涉及 ABI 扩展，越早越好 |
| 4 | 是否需要 `tsds_version()` 导出 | 需要 / 不需要 | 便于现场确认库版本，属 ABI 扩展 |
| 5 | 容量淘汰策略 | 保持近似 / 改为按时间区间 | 影响多通道长尾场景的正确性 |
| 6 | 是否补布隆过滤器 | 补 / 不补 | 读放大 vs 内存占用，等阶段 2 数据 |

---

## 附录 A：测试清单（81 条）

| 类别 | 文件 | 条数 |
| --- | --- | --- |
| 单元 | `checksum` | 4 |
| 单元 | `wal::record` | 6 |
| 单元 | `encoding` | 7 |
| 单元 | `config` | 4 |
| 单元 | `memtable` | 4 |
| 单元 | `sstable::block` | 2 |
| 单元 | `sstable::reader` | 4 |
| 单元 | `manifest` | 2 |
| 单元 | `iterator` | 2 |
| 单元 | `retention` | 2 |
| 单元 | `compaction`（限速） | 2 |
| 集成 | `tests/kv.rs` | 3 |
| 集成 | `tests/range.rs` | 4 |
| 集成 | `tests/wal_recovery.rs` | 4 |
| 集成 | `tests/flush.rs` | 6 |
| 集成 | `tests/ttl.rs` | 5 |
| 集成 | `tests/compaction.rs` | 5 |
| 集成 | `tests/background_compaction.rs` | 2 |
| 集成 | `tests/batch.rs` | 5 |
| 集成 | `tests/ffi.rs` | 6 |
| 集成 | `tests/smoke.rs` | 2 |
| **合计** | | **81** |

## 附录 B：交付物清单（阶段 1 现状）

```text
include/tsds.h          C ABI 契约（7 接口，签名与 v1.0 一致）
src/                    引擎内核 3,583 行
tests/                  11 个文件，42 条集成测试
c_demo/                 C 冒烟测试 + 构建脚本（待 Linux 验证）
docs/AIBOX-TSDS.md      v1.0 设计文档（归档）
docs/AIBOX-TSDS-v2.0.md 本文件
README.md               构建方式、语义约定、目录职责
```
