#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 加载仓库根目录的环境清单到当前 shell。
#
# 按顺序读取 .env（模板，入库）与 .env.local（本机私密值，gitignore），
# 后者覆盖前者；已存在的同名环境变量优先级最高，不会被这里覆盖。
#
# 用法（必须用 source，否则变量只存在于子进程里立刻消失）：
#     source tools/load-env.sh
#     cargo build
#
# 只想看实际会生效哪些值时：
#     bash tools/load-env.sh --print
# ---------------------------------------------------------------------------
set -e

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

print_mode=0
[ "${1:-}" = "--print" ] && print_mode=1

# $1 = 文件名；$2 = 'force' 时覆盖已存在的同名环境变量。
# .env 是共享模板，不优先生效；.env.local 是本项目显式配置，必须压过机器上的遗留值
# （典型场景：系统装了 libclang 22，会覆盖成本仓库要求的 18）。
load_file() {
    local path="$repo_root/$1"
    local force=0
    [ "${2:-}" = "force" ] && force=1
    [ -f "$path" ] || return 0

    while IFS= read -r line || [ -n "$line" ]; do
        # 去首尾空白
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        # 跳过空行与注释
        [ -z "$line" ] && continue
        case "$line" in \#*) continue ;; esac
        case "$line" in *=*) ;; *) continue ;; esac

        local name value
        name="${line%%=*}"
        value="${line#*=}"
        name="${name#"${name%%[![:space:]]*}"}"
        name="${name%"${name##*[![:space:]]}"}"
        value="${value#"${value%%[![:space:]]*}"}"
        value="${value%"${value##*[![:space:]]}"}"

        # 去成对引号
        case "$value" in
            \"*\") value="${value%\"}"; value="${value#\"}" ;;
            \'*\') value="${value%\'}"; value="${value#\'}" ;;
        esac

        [ -z "$value" ] && continue

        # 已存在的环境变量优先，不覆盖（force 模式除外）
        if [ "$force" = "0" ] && [ -n "${!name:-}" ]; then
            continue
        fi

        if [ "$print_mode" = "1" ]; then
            printf '%s=%s\n' "$name" "$value"
        else
            export "$name=$value"
        fi
    done < "$path"
}

if [ "$print_mode" = "1" ]; then
    load_file ".env"
    load_file ".env.local" force
else
    set -a
    load_file ".env"
    load_file ".env.local" force
    set +a
    echo "[load-env] 已从 .env / .env.local 加载环境变量。"
fi
