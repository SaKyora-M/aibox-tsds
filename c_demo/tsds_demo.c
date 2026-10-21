/* tsds_demo.c —— AIBOX-TSDS C ABI 冒烟测试
 *
 * 编译（Linux / WSL2）:
 *     gcc -std=c11 -O2 -I ../include tsds_demo.c -L ../target/release -ltsds -o tsds_demo
 *     LD_LIBRARY_PATH=../target/release ./tsds_demo
 *
 * 编译（Windows / MSVC，需先有 tsds.dll + tsds.lib）:
 *     cl /I ..\include tsds_demo.c ..\target\release\tsds.lib
 */
#include <stdio.h>
#include <string.h>
#include <stdint.h>

#include "tsds.h"

static int failures = 0;

static void check(int condition, const char *what) {
    if (condition) {
        printf("  ok   %s\n", what);
    } else {
        printf("  FAIL %s\n", what);
        failures += 1;
    }
}

int main(void) {
    const char *dir = "/tmp/tsds-demo";

    tsds_db *db = tsds_open(dir, 0);
    if (db == NULL) {
        printf("tsds_open(%s) 失败\n", dir);
        return 1;
    }
    printf("打开实例: %s\n", dir);

    /* ---- 单条写入 ---- */
    const char *v1 = "person";
    const char *v2 = "intrusion";
    check(tsds_put(db, 1, 1000, NULL, 0, (const uint8_t *)v1, strlen(v1)) == 0,
          "tsds_put 通道1 ts=1000");
    check(tsds_put(db, 1, 2000, NULL, 0, (const uint8_t *)v2, strlen(v2)) == 0,
          "tsds_put 通道1 ts=2000");

    /* ---- 批量写入 ---- */
    const char *k0 = "alarm:1", *b0 = "fire";
    const char *k1 = "alarm:2", *b1 = "smoke";
    tsds_item items[2];
    memset(items, 0, sizeof(items));
    items[0].channel = 2;
    items[0].timestamp_ms = 3000;
    items[0].key = (const uint8_t *)k0;
    items[0].key_len = strlen(k0);
    items[0].value = (const uint8_t *)b0;
    items[0].value_len = strlen(b0);
    items[1].channel = 2;
    items[1].timestamp_ms = 4000;
    items[1].key = (const uint8_t *)k1;
    items[1].key_len = strlen(k1);
    items[1].value = (const uint8_t *)b1;
    items[1].value_len = strlen(b1);
    check(tsds_batch_put(db, items, 2) == 0, "tsds_batch_put 两条");

    /* ---- 范围查询 ---- */
    tsds_iter *it = tsds_query(db, 1, 0, INT64_MAX);
    check(it != NULL, "tsds_query 通道1 全区间");

    int count = 0;
    int ordered = 1;
    int64_t previous = 0;
    if (it != NULL) {
        for (;;) {
            int64_t ts = 0;
            const uint8_t *value = NULL;
            size_t len = 0;
            int rc = tsds_iter_next(it, &ts, &value, &len);
            if (rc == 0) {
                break;
            }
            if (rc < 0) {
                printf("  FAIL tsds_iter_next 返回 %d\n", rc);
                failures += 1;
                break;
            }
            if (count > 0 && ts <= previous) {
                ordered = 0;
            }
            previous = ts;
            printf("  记录 ts=%lld value=%.*s\n", (long long)ts, (int)len,
                   (const char *)value);
            count += 1;
        }
        tsds_iter_free(it);
    }
    check(count == 2, "通道1 命中 2 条");
    check(ordered, "结果按时间升序");

    /* ---- 边界参数 ---- */
    check(tsds_query(db, 1, 100, 10) == NULL, "start > end 返回 NULL");
    check(tsds_put(NULL, 1, 0, NULL, 0, NULL, 0) < 0, "NULL 句柄写入被拒绝");

    tsds_close(db);

    /* ---- 重开验证持久化 ---- */
    db = tsds_open(dir, 0);
    check(db != NULL, "重新打开实例");
    if (db != NULL) {
        it = tsds_query(db, 2, 0, INT64_MAX);
        count = 0;
        if (it != NULL) {
            for (;;) {
                int64_t ts = 0;
                const uint8_t *value = NULL;
                size_t len = 0;
                if (tsds_iter_next(it, &ts, &value, &len) != 1) {
                    break;
                }
                count += 1;
            }
            tsds_iter_free(it);
        }
        check(count == 2, "重开后通道2 仍在（WAL 重放）");
        tsds_close(db);
    }

    printf("\n%s（失败 %d 项）\n", failures == 0 ? "全部通过" : "存在失败", failures);
    return failures == 0 ? 0 : 1;
}
