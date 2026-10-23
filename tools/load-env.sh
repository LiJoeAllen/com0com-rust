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

# 把指向仓库内、真实存在的相对路径转成绝对路径（Windows 风格、正斜杠）。
# 必须这么做：wdk-build 的 build.rs 以包目录（crates/<driver>）为 cwd 运行，
# .env 里的 target/... 相对路径在它眼里指向 crates/<driver>/target/...，必然失效。
absolutize() {
    local v="$1"
    case "$v" in
        [A-Za-z]:[\\/]*|/*) printf '%s' "$v"; return ;;
    esac
    if [ -e "$repo_root/$v" ]; then
        cygpath -m "$repo_root/$v" 2>/dev/null || printf '%s' "$repo_root/$v"
    else
        printf '%s' "$v"
    fi
}

# 输出目录（测试包、测试结果）可能尚不存在，不能用存在性判断；
# 相对值一律按仓库根锚定。
absolutize_output() {
    local v="$1"
    case "$v" in
        [A-Za-z]:[\\/]*|/*) printf '%s' "$v" ;;
        *) cygpath -m "$repo_root/$v" 2>/dev/null || printf '%s' "$repo_root/$v" ;;
    esac
}

# BINDGEN_EXTRA_CLANG_ARGS 按空白切分，把每个 -I 的目标路径做同样的锚定
#（该变量不允许含空格路径，见 .env 说明，因此按空白切分是安全的）。
absolutize_bindgen_args() {
    local out="" tok prev_I=0
    for tok in $1; do
        if [ "$prev_I" = "1" ]; then
            tok="$(absolutize "$tok")"; prev_I=0
        elif [ "${tok#-I}" != "$tok" ] && [ "${tok#-I}" != "" ]; then
            tok="-I$(absolutize "${tok#-I}")"
        elif [ "$tok" = "-I" ]; then
            prev_I=1
        fi
        out="$out$tok "
    done
    printf '%s' "${out% }"
}

# 按变量类型做路径锚定；其余变量原样返回。
resolve_value() {
    local name="$1" value="$2"
    case "$name" in
        LIBCLANG_PATH|WDKContentRoot|DEVCON_PATH|WDK_BIN_PATH|VMRUN_PATH)
            printf '%s' "$(absolutize "$value")" ;;
        VM_TEST_PACKAGE_DIR|VM_TEST_RESULTS_DIR)
            printf '%s' "$(absolutize_output "$value")" ;;
        BINDGEN_EXTRA_CLANG_ARGS)
            printf '%s' "$(absolutize_bindgen_args "$value")" ;;
        *)
            printf '%s' "$value" ;;
    esac
}

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
            value="$(resolve_value "$name" "$value")"
            # 口令等敏感值不回显。
            if [ "$name" = "VMRUN_GUEST_PASSWORD" ]; then value="***"; fi
            printf '%s=%s\n' "$name" "$value"
        else
            value="$(resolve_value "$name" "$value")"
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

    # signtool 必须压过 Git Bash / MSYS PATH 里的同名第三方程序（它会报
    # "unrecognized option: sign"）。auto-detect 由 build-package.ps1 负责。
    if [ -n "${WDK_BIN_PATH:-}" ]; then
        if [ ! -f "$WDK_BIN_PATH/signtool.exe" ]; then
            echo "[load-env] 警告：WDK_BIN_PATH 下没有 signtool.exe：$WDK_BIN_PATH" >&2
        else
            case ":$PATH:" in
                *":$WDK_BIN_PATH:"*) ;;
                *) export PATH="$WDK_BIN_PATH:$PATH" ;;
            esac
        fi
    fi
fi
