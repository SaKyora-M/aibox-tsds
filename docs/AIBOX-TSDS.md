# 《AIBOX-TSDS 轻量级时序数据存储引擎技术方案书》

## 一、项目简介

AIBOX-TSDS 是一款基于 Rust + LSM-Tree 实现的轻量级时序数据存储引擎，运行于边缘 AI 盒子（如 AX650N，ARM64，eMMC/SSD 存储，内存受限），用于存储录像索引、告警事件、AI 检测结果等结构化时序元数据。引擎编译为 `libtsds.so`，导出 C ABI，供现有 C++ 算法插件通过 FFI 直接调用，不改动现有软件系统架构。

**职责边界：**

| ✅ 存什么 | ❌ 不存什么 |
| --- | --- |
| 录像文件的索引与元数据（文件路径、起止时间段、通道号等） | 视频文件本身（仍走现有文件系统方案） |
| 告警事件记录（时间、类型、等级、描述等结构化字段） | 告警关联的图片 / 抓拍等大对象（仅存路径引用） |
| AI 检测结果等结构化时序元数据 | 任何非结构化二进制大对象 |

**技术特性：**

| 项目 | 说明 |
| --- | --- |
| 实现语言 | Rust |
| 存储架构 | LSM-Tree：MemTable + SSTable + WAL |
| 接口形式 | 动态库 `libtsds.so`，导出 C ABI，C++ 插件经 FFI 调用 |
| 资源占用 | 内存峰值可配置，默认 64MB，适配内存受限的边缘设备 |
| 写模型 | 追加写 + WAL，掉电不丢已确认数据 |

## 二、核心能力

### 2.1 时间范围查询

数据按 `(通道, 时间戳)` 有序组织，同一通道内的记录按时间戳递增排列，支持按时间区间高效扫描。查询返回流式迭代器，调用方逐条读取，结果集大小不受内存限制。

伪代码示例：查询某摄像头某时段的告警事件。

```text
# 查询通道 3 在 [t1, t2) 时段内的全部告警事件
db   = tsds_open("/data/tsds", 0)                 # 0 表示使用默认内存配置
iter = tsds_query(db, channel = 3,
                  start_ms = t1, end_ms = t2)     # 左闭右开时间区间
while tsds_iter_next(iter, &ts, &value):          # 流式逐条读取
    process(ts, value)                            # 处理该条告警事件
tsds_iter_free(iter)
tsds_close(db)
```

### 2.2 数据生命周期管理

- **TTL 自动过期**：记录可携带生存时间，到期后由引擎自动清理，无需上层干预。
- **容量淘汰策略**：存储占用超过阈值时触发淘汰，策略可配置：
  - **默认：删除最旧数据**——按时间顺序淘汰最旧记录，兼容现有循环覆盖逻辑；
  - **扩展：分级保留**——按事件等级 / 类型设置差异化保留时长，高等级事件保留更久。

### 2.3 批量写入

提供 `tsds_batch_put` 接口，将多条记录合并为一次提交：一次 WAL 追加、一次刷盘，显著降低高频事件场景下的日志与 IO 开销。算法插件在事件密集时段应优先使用批量接口，而非逐条 `tsds_put`。

### 2.4 快照与恢复

- **在线快照**：运行中可将当前数据一致性导出为快照文件，用于备份或迁移，不阻塞正常读写。
- **崩溃恢复**：进程异常退出或设备掉电后，重启时引擎自动重放 WAL，恢复已确认写入的数据，无需人工介入。

## 三、集成方法

### 3.1 交付物清单

| 交付物 | 说明 |
| --- | --- |
| `libtsds.so` | 动态库，提供 aarch64 / x86_64 两个架构版本 |
| `libtsds.a` | 静态库（可选，用于静态链接场景） |
| `tsds.h` | C ABI 头文件，声明全部导出接口与数据结构 |

### 3.2 C 接口定义

```c
/* tsds.h —— AIBOX-TSDS C ABI 接口定义 */

#include <stdint.h>
#include <stddef.h>

typedef struct tsds_db   tsds_db;    /* 引擎实例句柄（不透明类型） */
typedef struct tsds_iter tsds_iter;  /* 查询迭代器句柄（不透明类型） */

/* 批量写入条目 */
typedef struct {
    uint32_t        channel;       /* 通道号 */
    int64_t         timestamp_ms;  /* 时间戳（毫秒） */
    const uint8_t*  key;           /* 键数据指针 */
    size_t          key_len;       /* 键长度（字节） */
    const uint8_t*  value;         /* 值数据指针 */
    size_t          value_len;     /* 值长度（字节），建议 <= 4KB */
} tsds_item;

/*
 * 打开（不存在则创建）一个 TSDS 实例
 * path         : 数据目录路径，如 "/data/tsds"
 * mem_limit_mb : 内存峰值上限（MB），传 0 使用默认值 64
 * 返回         : 成功返回实例句柄，失败返回 NULL
 */
tsds_db* tsds_open(const char* path, uint32_t mem_limit_mb);

/*
 * 写入单条记录
 * db           : tsds_open 返回的实例句柄
 * channel      : 通道号
 * timestamp_ms : 时间戳（毫秒）
 * key / key_len: 键数据指针与长度
 * value / value_len : 值数据指针与长度，建议 <= 4KB
 * 返回         : 0 成功（数据已入 WAL，掉电不丢），非 0 失败
 */
int tsds_put(tsds_db* db, uint32_t channel, int64_t timestamp_ms,
             const uint8_t* key, size_t key_len,
             const uint8_t* value, size_t value_len);

/*
 * 时间范围查询，返回流式迭代器
 * db       : 实例句柄
 * channel  : 通道号
 * start_ms : 查询起始时间戳（毫秒，含）
 * end_ms   : 查询结束时间戳（毫秒，不含），区间为 [start_ms, end_ms)
 * 返回     : 迭代器指针，失败返回 NULL；用毕须调 tsds_iter_free 释放
 */
tsds_iter* tsds_query(tsds_db* db, uint32_t channel,
                      int64_t start_ms, int64_t end_ms);

/*
 * 迭代器前进一步，读取当前记录
 * it              : tsds_query 返回的迭代器
 * timestamp_ms_out: 输出参数，返回当前记录时间戳
 * value_out / value_len_out : 输出参数，返回当前记录值指针与长度
 *                  （指向下一次调用 tsds_iter_next 前有效，无需释放）
 * 返回     : 1 读到一条记录；0 迭代结束；<0 出错
 */
int tsds_iter_next(tsds_iter* it, int64_t* timestamp_ms_out,
                   const uint8_t** value_out, size_t* value_len_out);

/*
 * 释放迭代器
 * it : 待释放的迭代器，允许传 NULL
 */
void tsds_iter_free(tsds_iter* it);

/*
 * 批量写入：多条记录合并为一次提交（一次 WAL 追加、一次刷盘）
 * db    : 实例句柄
 * items : 批量写入条目数组
 * count : 条目数量
 * 返回  : 0 全部成功；>0 前 N 条成功、第 N 条起失败；<0 整体失败
 */
int tsds_batch_put(tsds_db* db, const tsds_item* items, size_t count);

/*
 * 关闭实例，刷清缓存并释放全部资源
 * db : 实例句柄，关闭后不得再使用
 */
void tsds_close(tsds_db* db);
```

### 3.3 存储路径约定

引擎数据统一存放于 `/data/tsds/` 目录：

```text
/data/tsds/
├── tsds.conf          # 配置文件：内存上限、TTL、淘汰策略等
├── wal/               # WAL 预写日志目录
│   └── 000001.log
├── sst/               # SSTable 数据文件目录（按层级组织）
│   ├── L0/
│   └── L1/
└── snapshot/          # 在线快照输出目录
```

## 四、与现有系统接入

### 4.1 接入原则

- TSDS 以独立动态库形式被插件进程加载，不改现有软件架构，不改现有视频文件存储链路；
- 支持从单个插件试点接入，验证稳定后逐步推广至其他插件，各插件互不耦合。

### 4.2 典型接入场景（分阶段）

**场景 A：告警事件存储（推荐首期）**

算法插件检测到告警后，通过 `tsds_batch_put` 将事件（时间、通道、类型、等级等字段）批量写入 TSDS；管理平台通过 `tsds_query` 按时间范围拉取事件列表。该场景写入频次高、单条数据小，与引擎的批量写与范围查询能力直接匹配，适合作为首期验证场景。

**场景 B：录像索引管理（二期）**

将录像片段的 `(通道, 起止时间, 文件路径)` 索引由现有 SQLite 表迁移至 TSDS；录像文件仍存于文件系统，TSDS 仅记录其索引与元数据。录像覆盖时，同步删除 TSDS 中的索引与文件系统中的对应文件，保证两者一致。

**场景 C：AI 检测结果时序化（可选）**

将 AI 检测结果按时间序写入 TSDS，与录像索引共用时间轴，支撑"检索含人形告警的录像片段"类智能查询：先按条件命中检测结果的时间段，再关联录像索引定位文件。

### 4.3 对现有组件的影响

| 组件 | 影响 |
| --- | --- |
| 视频文件存储 | 无改动，仍走现有文件系统方案 |
| 现有 SQLite 索引 | 验证后逐步替换（场景 B），替换期间双跑比对 |
| C++ 算法插件 | 新增对 `libtsds.so` 的链接与 FFI 调用 |
| 管理平台 | 查询逻辑改为调用 TSDS 接口 |

## 五、约束与注意事项

1. 单条 value 建议 **≤ 4KB**；大文件写入文件系统，TSDS 仅记录其路径。
2. 默认配置支撑**每秒数千次**写入；更高负载请使用 `tsds_batch_put` 批量提交。
3. 掉电语义：`tsds_put` / `tsds_batch_put` 返回成功即表示数据已写入 WAL，掉电不丢；未返回的写入可能丢失。
4. 建议数据分区使用 **ext4** 文件系统。
