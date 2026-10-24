#!/usr/bin/env bash
#
# Linux/macOS 纯 Rust PTY 后端端到端验收脚本。
#
# 覆盖：Unix socket 上的守护进程启动、显式/自动端口标签、稳定的 /dev 风格
# 符号链接、双向 4096 字节传输、端口参数、波特率节流、噪声注入、overrun
# 丢字节、能力位诚实拒绝、经 sdk-smoke 验证 Monitor/事件、daemon 重启
# --restore 恢复，以及最终清理。
#
# 每条断言向日志和标准输出写 `PASS| ...` / `FAIL| ...`，最后一行是
# `RESULT| PASS|FAIL checks=N failures=M`，与 Windows guest 脚本保持一致，
# 宿主机解析器可复用同一套规则。
#
# 用法：
#   check-pairs-unix.sh [--base-dir DIR] [--log PATH] [--keep]
#
#   --base-dir  setupc、com0comd、sdk-smoke 所在目录（默认脚本自身目录）
#   --log       断言日志路径（默认 <工作目录>/check-pairs-unix.log）
#   --keep      保留临时工作目录，便于排查失败
#
# 环境变量覆盖：SETUPC、COM0COMD、SDK_SMOKE、PYTHON3。

set -u

BASE_DIR="$(cd "$(dirname "$0")" && pwd)"
LOG_PATH=""
KEEP=0

while [ $# -gt 0 ]; do
    case "$1" in
        --base-dir)
            [ $# -ge 2 ] || { echo "check-pairs-unix: --base-dir 需要值" >&2; exit 2; }
            BASE_DIR="$2"
            shift 2
            ;;
        --log)
            [ $# -ge 2 ] || { echo "check-pairs-unix: --log 需要值" >&2; exit 2; }
            LOG_PATH="$2"
            shift 2
            ;;
        --keep)
            KEEP=1
            shift
            ;;
        -h|--help)
            sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "check-pairs-unix: 未知参数: $1" >&2
            exit 2
            ;;
    esac
done

SETUPC="${SETUPC:-$BASE_DIR/setupc}"
COM0COMD="${COM0COMD:-$BASE_DIR/com0comd}"
SDK_SMOKE="${SDK_SMOKE:-$BASE_DIR/sdk-smoke}"
PYTHON3="${PYTHON3:-python3}"

# 临时目录隔离 endpoint、状态文件和端口链接，避免污染宿主 /dev 或用户目录。
WORK="$(mktemp -d "${TMPDIR:-/tmp}/com0com-e2e.XXXXXX")" || exit 1
[ -n "$LOG_PATH" ] || LOG_PATH="$WORK/check-pairs-unix.log"
: > "$LOG_PATH"

DEV_DIR="$WORK/dev"
SOCK="$WORK/control.sock"
STATE="$WORK/state.json"
DAEMON_LOG="$WORK/com0comd.log"
PTY_IO="$WORK/pty_io.py"
DAEMON_PID=""
ROOT_PID=""
ROOT_LINK_A=""
ROOT_LINK_B=""
ROOT_ALIAS_A=""

CHECKS=0
FAILURES=0
EXIT_CODE=1

# 同时写日志与终端；宿主机按 `PASS|`/`RESULT|` 前缀解析。
log_line() {
    printf '%s %s\n' "$(date '+%H:%M:%S')" "$1" | tee -a "$LOG_PATH"
}

cleanup() {
    if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    if [ -n "$ROOT_PID" ] && kill -0 "$ROOT_PID" 2>/dev/null; then
        kill "$ROOT_PID" 2>/dev/null || true
        wait "$ROOT_PID" 2>/dev/null || true
    fi
    # root 段失败时清理：只删本脚本创建的符号链接，普通文件一律不动。
    for link in "$ROOT_LINK_A" "$ROOT_LINK_B" "$ROOT_ALIAS_A"; do
        [ -n "$link" ] && [ -L "$link" ] && rm -f "$link"
    done
    if [ "$KEEP" -eq 0 ]; then
        rm -rf "$WORK"
    else
        echo "check-pairs-unix: 已保留工作目录 $WORK"
    fi
}
trap cleanup EXIT

check() {
    # check <名称> <0|1> [详情]
    CHECKS=$((CHECKS + 1))
    local name="$1" ok="$2" detail="${3:-}"
    if [ -n "$detail" ]; then
        detail=" | $(printf '%s' "$detail" | tr '\n' ' ' | cut -c1-200)"
    fi
    if [ "$ok" -eq 1 ]; then
        log_line "PASS| $name$detail"
    else
        FAILURES=$((FAILURES + 1))
        log_line "FAIL| $name$detail"
    fi
}

# 失败时输出最有价值的两行，避免日志被刷屏。
brief() {
    if [ -f "$1" ]; then
        tr -d '\r' < "$1" | sed '/^[[:space:]]*$/d' | head -n 2 | paste -sd ' /' -
    fi
}

run_tool() {
    # run_tool <标签> <可执行文件> [参数...]：结果写入 RC 与 $WORK/<标签>.{out,err}
    local tag="$1"; shift
    local requested="$1"; shift
    local exe="$requested"
    case "$exe" in
        */*) ;;
        *) exe="$(command -v "$exe" 2>/dev/null || true)" ;;
    esac
    OUT="$WORK/$tag.out"
    ERR="$WORK/$tag.err"
    if [ -z "$exe" ] || [ ! -x "$exe" ]; then
        log_line "$tag| 缺少可执行文件 $requested"
        RC=127
        : > "$OUT"
        echo "missing $requested" > "$ERR"
        return 0
    fi
    "$exe" "$@" > "$OUT" 2> "$ERR"
    RC=$?
    return 0
}

setupc() {
    local tag="$1"; shift
    run_tool "setupc-$tag" "$SETUPC" "$@"
    OUT="$WORK/setupc-$tag.out"
    ERR="$WORK/setupc-$tag.err"
}

# 返回当前平台的稳定端口链接路径：Linux 为 <dir>/<label>，
# macOS 为 <dir>/cu.com0com-<label>。
port_link() {
    if [ "$(uname -s)" = "Darwin" ]; then
        printf '%s/cu.com0com-%s' "$DEV_DIR" "$1"
    else
        printf '%s/%s' "$DEV_DIR" "$1"
    fi
}

# macOS 应用有时坚持打开阻塞式 tty.* 节点，因此宿主机额外创建别名。
port_alias() {
    if [ "$(uname -s)" = "Darwin" ]; then
        printf '%s/tty.com0com-%s' "$DEV_DIR" "$1"
    else
        printf ''
    fi
}

wait_daemon_status() {
    local timeout="${1:-20}" socket="${2:-$SOCK}" i=0
    while [ "$i" -lt "$timeout" ]; do
        if "$COM0COMD" status --pipe "$socket" > "$WORK/status.out" 2>&1; then
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done
    return 1
}

start_daemon() {
    "$COM0COMD" run --pipe "$SOCK" --restore --autosave --log "$DAEMON_LOG" \
        > "$WORK/daemon.out" 2>&1 &
    DAEMON_PID=$!
    wait_daemon_status 20
}

stop_daemon() {
    if [ -z "$DAEMON_PID" ]; then
        return 0
    fi
    "$COM0COMD" stop --pipe "$SOCK" > "$WORK/stop.out" 2>&1
    local rc=$?
    local i=0
    while [ "$i" -lt 100 ]; do
        if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
            break
        fi
        i=$((i + 1))
        sleep 0.1
    done
    wait "$DAEMON_PID" 2>/dev/null || true
    DAEMON_PID=""
    return "$rc"
}

# 生成 PTY 读写辅助脚本。用 python3 标准库直接操作 termios：
# - transfer：原始模式 + 可选接收端波特率，校验双向字节一致；
# - noise：发送全零，校验长度不变但内容被翻转；
# - overrun：对端不读时灌入大块数据，校验确实发生丢字节。
write_pty_helper() {
    cat > "$PTY_IO" <<'PY'
"""PTY 端到端 I/O 辅助：原始 termios、限时写入/读取、波特率设置。"""
import os
import select
import sys
import termios
import time

OPEN_FLAGS = os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK


def open_port(path):
    """以非阻塞方式打开 PTY 从端并切换为 8N1 原始模式。"""
    fd = os.open(path, OPEN_FLAGS)
    attrs = termios.tcgetattr(fd)
    attrs[0] = 0  # iflag：关闭 CR/LF 转换
    attrs[1] = 0  # oflag
    attrs[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    attrs[3] = 0  # lflag：关闭回显与行缓冲
    attrs[6][termios.VMIN] = 1
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, attrs)
    return fd


def set_speed(fd, name):
    """设置设备波特率；pump 通过 cfgetospeed 读取该值做节流。"""
    attrs = termios.tcgetattr(fd)
    speed = getattr(termios, name)
    attrs[4] = speed
    attrs[5] = speed
    termios.tcsetattr(fd, termios.TCSANOW, attrs)


def write_all(fd, data, timeout):
    """在超时内尽可能写完，返回实际写入字节数。"""
    deadline = time.monotonic() + timeout
    offset = 0
    while offset < len(data):
        left = deadline - time.monotonic()
        if left <= 0:
            break
        _, writable, _ = select.select([], [fd], [], left)
        if not writable:
            continue
        try:
            offset += os.write(fd, data[offset:])
        except BlockingIOError:
            time.sleep(0.005)
        except OSError as error:
            if error.errno in (11, 35):  # EAGAIN / EWOULDBLOCK
                time.sleep(0.005)
                continue
            raise
    return offset


def read_up_to(fd, want, timeout):
    """在超时内读取至多 want 字节；超时返回已读内容而不是报错。"""
    deadline = time.monotonic() + timeout
    out = bytearray()
    while len(out) < want:
        left = deadline - time.monotonic()
        if left <= 0:
            break
        readable, _, _ = select.select([fd], [], [], left)
        if not readable:
            continue
        try:
            chunk = os.read(fd, min(4096, want - len(out)))
        except BlockingIOError:
            continue
        except OSError as error:
            if error.errno in (11, 35):
                continue
            raise
        if not chunk:
            break
        out.extend(chunk)
    return bytes(out)


def transfer(fd_a, fd_b, payload, timeout):
    """边写边读：先写满再读会在 macOS 的 2 KiB PTY 缓冲上互相等待。"""
    deadline = time.monotonic() + timeout
    sent = 0
    out = bytearray()
    while (sent < len(payload) or len(out) < len(payload)) and time.monotonic() < deadline:
        left = max(0.0, deadline - time.monotonic())
        writable_fds = [fd_a] if sent < len(payload) else []
        readable, writable, _ = select.select([fd_b], writable_fds, [], left)
        if readable:
            try:
                chunk = os.read(fd_b, 4096)
            except BlockingIOError:
                chunk = b""
            except OSError as error:
                if error.errno in (11, 35):
                    chunk = b""
                else:
                    raise
            if chunk:
                out.extend(chunk)
        if writable:
            try:
                sent += os.write(fd_a, payload[sent:sent + 4096])
            except BlockingIOError:
                pass
            except OSError as error:
                if error.errno not in (11, 35):
                    raise
    return sent, bytes(out)


def main():
    mode, a_path, b_path = sys.argv[1], sys.argv[2], sys.argv[3]
    size = int(sys.argv[4])
    fd_a = open_port(a_path)
    fd_b = open_port(b_path)
    try:
        if mode == "transfer":
            if len(sys.argv) > 5 and sys.argv[5]:
                set_speed(fd_b, sys.argv[5])
            payload = bytes((i * 31 + 7) & 0xFF for i in range(size))
            start = time.monotonic()
            sent, received = transfer(fd_a, fd_b, payload, 30)
            elapsed = time.monotonic() - start
            if sent != size:
                print(f"short-write sent={sent} want={size}")
                return 1
            if len(received) != size:
                print(f"short-read received={len(received)} want={size}")
                return 1
            if received != payload:
                print(f"mismatch received={len(received)} want={size}")
                return 1
            print(f"ok bytes={len(received)} elapsed={elapsed:.3f}")
            return 0

        if mode == "noise":
            payload = bytes(size)
            sent = write_all(fd_a, payload, 10)
            received = read_up_to(fd_b, sent, 10)
            if sent != size or len(received) != sent:
                print(f"length received={len(received)} sent={sent} want={size}")
                return 1
            if received == payload:
                print("noise did not alter any byte")
                return 1
            print(f"ok bytes={len(received)} altered")
            return 0

        if mode == "overrun":
            hold_ms = int(sys.argv[5]) if len(sys.argv) > 5 else 1500
            payload = bytes((i * 17 + 3) & 0xFF for i in range(size))
            sent = write_all(fd_a, payload, 30)
            # 故意延迟读取，让目标端内核缓冲填满并触发 overrun 丢弃。
            time.sleep(hold_ms / 1000.0)
            received = read_up_to(fd_b, sent, 5)
            print(f"ok sent={sent} received={len(received)}")
            return 0

        print(f"unknown mode: {mode}")
        return 2
    finally:
        os.close(fd_a)
        os.close(fd_b)


if __name__ == "__main__":
    sys.exit(main())
PY
}

py_run() {
    local tag="$1"; shift
    run_tool "$tag" "$PYTHON3" "$PTY_IO" "$@"
    OUT="$WORK/$tag.out"
    ERR="$WORK/$tag.err"
}

log_line "=== check-pairs-unix $(date '+%Y-%m-%dT%H:%M:%S%z') ==="
log_line "base-dir=$BASE_DIR work=$WORK"

for tool in "$SETUPC" "$COM0COMD" "$SDK_SMOKE"; do
    check "bin| exists $(basename "$tool")" "$([ -x "$tool" ] && echo 1 || echo 0)" "$tool"
done
check "bin| python3 available" "$("$PYTHON3" -c 'import termios, select' >/dev/null 2>&1 && echo 1 || echo 0)" "$PYTHON3"

if ! command -v "$PYTHON3" >/dev/null 2>&1; then
    log_line "RESULT| FAIL checks=$CHECKS failures=$FAILURES"
    exit 1
fi
write_pty_helper

mkdir -p "$DEV_DIR"
# 三个环境变量把 daemon、状态和链接全部限制在临时目录内。
export COM0COM_DEV_DIR="$DEV_DIR"
export COM0COM_ENDPOINT="$SOCK"
export COM0COM_STATE="$STATE"
unset COM0COM_BACKEND

# ---------------------------------------------------------------------------
# 守护进程启动
# ---------------------------------------------------------------------------
start_daemon
check "daemon| started with --restore --autosave" "$([ $? -eq 0 ] && echo 1 || echo 0)" "$(brief "$WORK/status.out")"
check "daemon| control socket exists" "$([ -S "$SOCK" ] && echo 1 || echo 0)" "$SOCK"

run_tool selftest "$COM0COMD" selftest
check "daemon| selftest" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

# ---------------------------------------------------------------------------
# 建对、标签与符号链接
# ---------------------------------------------------------------------------
setupc add0 add --id 0
check "add| pair 0 created" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
LINK_A0="$(port_link A0)"
LINK_B0="$(port_link B0)"
check "link| A0 exists" "$([ -L "$LINK_A0" ] && echo 1 || echo 0)" "$LINK_A0"
check "link| B0 exists" "$([ -L "$LINK_B0" ] && echo 1 || echo 0)" "$LINK_B0"
TARGET_A0="$(readlink "$LINK_A0" 2>/dev/null || true)"
case "$TARGET_A0" in
    /dev/pts/*|/dev/ttys*|/dev/pty*) check "link| A0 points at a PTY node" 1 "$TARGET_A0" ;;
    *) check "link| A0 points at a PTY node" 0 "$TARGET_A0" ;;
esac
if [ "$(uname -s)" = "Darwin" ]; then
    ALIAS_A0="$(port_alias A0)"
    check "link| A0 tty alias exists" "$([ -L "$ALIAS_A0" ] && echo 1 || echo 0)" "$ALIAS_A0"
fi

setupc list0 list
check "list| pair 0 visible" "$(grep -Eq '^0[[:space:]]+A0[[:space:]]+B0' "$OUT" && echo 1 || echo 0)" "$(brief "$OUT")"

# 链接目录中的普通文件绝不能被符号链接覆盖，这是防止误删用户数据的安全线。
GUARD_LINK="$(port_link GUARD)"
printf 'guard' > "$GUARD_LINK"
setupc guard add --id 3 --port-a GUARD --port-b G3B
check "link| regular file blocks pair creation" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"
check "link| regular file untouched" "$([ "$(cat "$GUARD_LINK" 2>/dev/null)" = "guard" ] && echo 1 || echo 0)" "$GUARD_LINK"
rm -f "$GUARD_LINK"

# ---------------------------------------------------------------------------
# 数据传输
# ---------------------------------------------------------------------------
py_run xfer-ab transfer "$LINK_A0" "$LINK_B0" 4096
check "io| A->B 4096 bytes" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
py_run xfer-ba transfer "$LINK_B0" "$LINK_A0" 4096
check "io| B->A 4096 bytes" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

# ---------------------------------------------------------------------------
# 参数与用户态仿真
# ---------------------------------------------------------------------------
# pair 5 使用自动标签 A5/B5：`setupc set` 按旧 CLI 语义整体替换参数块，
# 显式标签会被重置，因此仿真测试放在这里，显式标签留给 pair 9。
setupc add5 add --id 5
check "add| pair 5 created" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
LINK_A5="$(port_link A5)"
LINK_B5="$(port_link B5)"
check "link| A5/B5 created" "$([ -L "$LINK_A5" ] && [ -L "$LINK_B5" ] && echo 1 || echo 0)"

setupc params5 params 5
check "params| labels reported" "$(grep -q 'PortA:      A5' "$OUT" && grep -q 'PortB:      B5' "$OUT" && echo 1 || echo 0)" "$(brief "$OUT")"

setupc set5baud set 5 --baud-emu
check "params| baud-emu accepted" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"
setupc params5baud params 5
check "params| baud-emu reported" "$(grep -q 'baud-emu' "$OUT" && echo 1 || echo 0)" "$(brief "$OUT")"
py_run baud transfer "$LINK_A5" "$LINK_B5" 1024 B9600
BAUD_OK=0
BAUD_ELAPSED=0
if [ "$RC" -eq 0 ]; then
    BAUD_ELAPSED="$(sed -n 's/.*elapsed=\([0-9.]*\).*/\1/p' "$OUT" | head -n 1)"
    # 1024 字节 @9600 8N1 约 1.07s；节流器允许 0.5s 突发，阈值取 0.4s。
    if [ -n "$BAUD_ELAPSED" ] && awk -v t="$BAUD_ELAPSED" 'BEGIN { exit !(t >= 0.4) }'; then
        BAUD_OK=1
    fi
fi
check "sim| 9600 baud throttles 1024 bytes" "$BAUD_OK" "elapsed=${BAUD_ELAPSED:-?}"

setupc set5noise set 5 --noise=10000
check "params| noise accepted" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"
py_run noise noise "$LINK_A5" "$LINK_B5" 1024
check "sim| 100% noise alters bytes without changing length" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

setupc set5overrun set 5 --overrun
check "params| overrun accepted" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"
py_run overrun overrun "$LINK_A5" "$LINK_B5" 131072 1500
OVERRUN_OK=0
if [ "$RC" -eq 0 ]; then
    SENT="$(sed -n 's/.*sent=\([0-9]*\).*/\1/p' "$OUT" | head -n 1)"
    RECEIVED="$(sed -n 's/.*received=\([0-9]*\).*/\1/p' "$OUT" | head -n 1)"
    if [ -n "$SENT" ] && [ -n "$RECEIVED" ] && [ "$SENT" -gt 0 ] && [ "$RECEIVED" -lt "$SENT" ]; then
        OVERRUN_OK=1
    fi
fi
check "sim| overrun drops unread bytes" "$OVERRUN_OK" "$(brief "$OUT")"

# PTY 无法提供的能力必须显式失败，不能静默伪造。
setupc exclusive set 5 --exclusive
check "caps| exclusive rejected" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"
setupc plugin set 5 --plugin
check "caps| plugin rejected" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"
setupc collision set 5 --port-a A0
check "caps| duplicate label rejected" "$([ "$RC" -ne 0 ] && echo 1 || echo 0)" "$(brief "$ERR")"

# 第三对验证自动标签路径（A7/B7）。
setupc add7 add --id 7
check "add| pair 7 auto labels" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
check "link| A7/B7 created" "$([ -L "$(port_link A7)" ] && [ -L "$(port_link B7)" ] && echo 1 || echo 0)"

# ---------------------------------------------------------------------------
# 经 SDK 验证 Monitor、事件与状态（进程内 PTY 与 daemon 两种模式）
# ---------------------------------------------------------------------------
# 进程内 PTY 后端使用独立的链接目录，避免与正在运行的 daemon 争用 A0/B0。
run_tool sdk-native env COM0COM_DEV_DIR="$WORK/native-dev" "$SDK_SMOKE" --native
check "sdk| native PTY smoke (monitor + events + state)" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

run_tool sdk-daemon "$SDK_SMOKE" --daemon "$SOCK"
check "sdk| daemon smoke over Unix socket" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

# ---------------------------------------------------------------------------
# daemon 重启与状态恢复
# ---------------------------------------------------------------------------
# 显式标签放在这里：该 pair 不再经过 `set`，标签不会被参数替换重置。
setupc add9 add --id 9 --port-a P9A --port-b P9B
check "add| pair 9 before restart" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
check "link| explicit labels created" "$([ -L "$(port_link P9A)" ] && [ -L "$(port_link P9B)" ] && echo 1 || echo 0)"
setupc params9 params 9
check "params| explicit labels reported" "$(grep -q 'PortA:      P9A' "$OUT" && grep -q 'PortB:      P9B' "$OUT" && echo 1 || echo 0)" "$(brief "$OUT")"
# 给 --autosave 的防抖窗口留出时间，确保退出前状态已落盘。
sleep 1

stop_daemon
STOP_RC=$?
check "daemon| stopped cleanly" "$([ "$STOP_RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$WORK/stop.out")"
check "state| autosave wrote state file" "$([ -s "$STATE" ] && echo 1 || echo 0)" "$STATE"
check "daemon| control socket removed" "$([ ! -e "$SOCK" ] && echo 1 || echo 0)" "$SOCK"
check "link| symlinks removed with the daemon" "$([ ! -e "$(port_link A0)" ] && echo 1 || echo 0)"

start_daemon
RESTART_RC=$?
check "daemon| restarted with --restore" "$([ "$RESTART_RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$WORK/status.out")"

setupc list-restored list
RESTORED=1
for id in 0 5 7 9; do
    grep -Eq "^$id[[:space:]]" "$OUT" || RESTORED=0
done
check "restore| pairs 0/5/7/9 recreated" "$RESTORED" "$(brief "$OUT")"
check "restore| explicit labels recreated" "$([ -L "$(port_link P9A)" ] && [ -L "$(port_link P9B)" ] && echo 1 || echo 0)"
py_run xfer-restored transfer "$(port_link P9A)" "$(port_link P9B)" 512
check "restore| restored pair carries data" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

# ---------------------------------------------------------------------------
# 清理
# ---------------------------------------------------------------------------
for id in 9 7 5 0; do
    setupc "remove$id" remove "$id"
    check "remove| pair $id removed" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
done

setupc list-final list
check "cleanup| final list empty" "$(grep -q '(no pairs)' "$OUT" && echo 1 || echo 0)" "$(brief "$OUT")"
check "cleanup| links removed" "$([ ! -e "$(port_link A0)" ] && [ ! -e "$(port_link A5)" ] && echo 1 || echo 0)"

stop_daemon
check "daemon| final stop" "$([ $? -eq 0 ] && echo 1 || echo 0)" "$(brief "$WORK/stop.out")"

# ---------------------------------------------------------------------------
# root 默认路径：仅在 root 下运行，验证 /dev 稳定链接与双向传输。
# 非 root 运行时整段跳过，前面的临时目录用例已经覆盖用户目录回退。
# ---------------------------------------------------------------------------
if [ "$(id -u)" -eq 0 ]; then
    ROOT_LABEL_A="RTA$$"
    ROOT_LABEL_B="RTB$$"
    if [ "$(uname -s)" = "Darwin" ]; then
        ROOT_LINK_A="/dev/cu.com0com-$ROOT_LABEL_A"
        ROOT_LINK_B="/dev/cu.com0com-$ROOT_LABEL_B"
        ROOT_ALIAS_A="/dev/tty.com0com-$ROOT_LABEL_A"
    else
        ROOT_LINK_A="/dev/com0com/$ROOT_LABEL_A"
        ROOT_LINK_B="/dev/com0com/$ROOT_LABEL_B"
    fi
    ROOT_SOCK="$WORK/root.sock"
    # 清掉临时链接目录覆盖，让 backend 走 root 默认路径；endpoint/状态仍隔离。
    unset COM0COM_DEV_DIR
    export COM0COM_ENDPOINT="$ROOT_SOCK" COM0COM_STATE="$WORK/root-state.json"

    "$COM0COMD" run --pipe "$ROOT_SOCK" --restore --autosave --log "$WORK/root.log" \
        > "$WORK/root-daemon.out" 2>&1 &
    ROOT_PID=$!
    wait_daemon_status 20 "$ROOT_SOCK"
    check "root| daemon started in default link dir" "$([ $? -eq 0 ] && echo 1 || echo 0)" "$(brief "$WORK/status.out")"

    run_tool root-add "$SETUPC" add --id 31 --port-a "$ROOT_LABEL_A" --port-b "$ROOT_LABEL_B"
    check "root| pair 31 created" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
    check "root| /dev links exist" "$([ -L "$ROOT_LINK_A" ] && [ -L "$ROOT_LINK_B" ] && echo 1 || echo 0)" "$ROOT_LINK_A"
    ROOT_TARGET="$(readlink "$ROOT_LINK_A" 2>/dev/null || true)"
    case "$ROOT_TARGET" in
        /dev/pts/*|/dev/ttys*|/dev/pty*) ROOT_TARGET_OK=1 ;;
        *) ROOT_TARGET_OK=0 ;;
    esac
    check "root| link points at a PTY node" "$ROOT_TARGET_OK" "$ROOT_TARGET"
    if [ -n "$ROOT_ALIAS_A" ]; then
        check "root| tty alias exists" "$([ -L "$ROOT_ALIAS_A" ] && echo 1 || echo 0)" "$ROOT_ALIAS_A"
    fi
    py_run root-xfer-ab transfer "$ROOT_LINK_A" "$ROOT_LINK_B" 512
    check "root| A->B 512 bytes" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
    py_run root-xfer-ba transfer "$ROOT_LINK_B" "$ROOT_LINK_A" 512
    check "root| B->A 512 bytes" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"

    run_tool root-remove "$SETUPC" remove 31
    check "root| pair removed" "$([ "$RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$OUT")$(brief "$ERR")"
    check "root| /dev links removed" "$([ ! -e "$ROOT_LINK_A" ] && [ ! -e "$ROOT_LINK_B" ] && echo 1 || echo 0)"
    "$COM0COMD" stop --pipe "$ROOT_SOCK" > "$WORK/root-stop.out" 2>&1
    ROOT_STOP_RC=$?
    wait "$ROOT_PID" 2>/dev/null || true
    ROOT_PID=""
    check "root| daemon stopped cleanly" "$([ "$ROOT_STOP_RC" -eq 0 ] && echo 1 || echo 0)" "$(brief "$WORK/root-stop.out")"
fi

# ---------------------------------------------------------------------------
# 汇总
# ---------------------------------------------------------------------------
if [ "$FAILURES" -eq 0 ]; then
    STATUS=PASS
    EXIT_CODE=0
else
    STATUS=FAIL
    EXIT_CODE=1
fi
log_line "RESULT| $STATUS checks=$CHECKS failures=$FAILURES"
log_line "=== done (exit $EXIT_CODE) ==="
exit "$EXIT_CODE"
