#!/usr/bin/env bash
# 在 Linux / WSL2 下构建并运行 C ABI 冒烟测试。
# 前置：cargo build --release 已产出 libtsds.so
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ROOT}/c_demo/tsds_demo"

echo "==> 目标库: ${ROOT}/target/release/libtsds.so"
test -f "${ROOT}/target/release/libtsds.so" || {
    echo "缺少 libtsds.so，请先运行 cargo build --release" >&2
    exit 1
}

echo "==> 编译 C demo"
gcc -std=c11 -O2 -Wall -Wextra \
    -I "${ROOT}/include" \
    "${ROOT}/c_demo/tsds_demo.c" \
    -L "${ROOT}/target/release" -ltsds \
    -o "${OUT}"

echo "==> 运行"
LD_LIBRARY_PATH="${ROOT}/target/release" "${OUT}"
