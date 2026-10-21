/* tsds.h —— AIBOX-TSDS C ABI 接口定义
 *
 * 与《AIBOX-TSDS 技术方案书》3.2 节一致，不改动任何签名。
 * 错误码约定（与实现一致）：
 *    0  成功
 *   -1  参数非法（NULL 指针、区间反向、value 超限、路径非法）
 *   -2  IO / 磁盘错误
 *   -3  数据损坏（WAL CRC、MANIFEST、SST 校验失败）
 *   -4  内部错误（后台线程 panic、句柄已关闭）
 * tsds_batch_put 额外支持 "> 0 表示前 N 条成功" 的部分成功语义。
 */
#ifndef TSDS_H
#define TSDS_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

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

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TSDS_H */
