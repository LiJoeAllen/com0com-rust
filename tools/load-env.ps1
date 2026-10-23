#!/usr/bin/env pwsh
<#
.SYNOPSIS
    加载仓库根目录的环境清单到当前会话。

    .DESCRIPTION
    按顺序读取 .env（团队模板，入库）与 .env.local（本机私密值，gitignore）。

    - .env        共享模板，遇到已存在的同名环境变量就跳过（不抢夺 CI 或命令行临时值）
    - .env.local  本项目显式配置，**覆盖**已存在的同名环境变量

    之所以区分：机器上常有遗留的同名变量挡路。典型的是系统装了 libclang 22，
    而本仓库内核 crate 必须用 18（22.1.x 的 bindgen 回归会让 wdk-sys 编译爆
    成片 E0080）。此时 .env.local 里写的值必须能压过系统变量。

    完整优先级：命令行显式传参 > .env.local > 已有环境变量 > .env > 脚本默认值

    用法（必须用 dot-source，否则变量只存在于子进程里立刻消失）：
        . tools/load-env.ps1
        pwsh -File tools/vm-test/run-vm-test.ps1

    只想看实际会生效哪些值时：
        pwsh -File tools/load-env.ps1 -Print
#>
param(
    [string]$RepoRoot = '',
    [switch]$Print,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

$effective = [ordered]@{}

# 把指向仓库内、真实存在的相对路径转成绝对路径（正斜杠，无空格）。
# 必须这么做：wdk-build 的 build.rs 以包目录（crates/<driver>）为 cwd 运行，
# .env 里的 target/... 相对路径在它眼里指向 crates/<driver>/target/...，必然失效。
function Resolve-RepoPath([string]$Value) {
    if ($Value -match '^([A-Za-z]:[\\/]|/)') { return $Value }
    $joined = Join-Path $RepoRoot $Value
    if (Test-Path -LiteralPath $joined) { return ($joined -replace '\\', '/') }
    return $Value
}

# 输出目录（测试包、测试结果）可能尚不存在，不能用 Test-Path 判断；
# 相对值一律按仓库根锚定。
function Resolve-RepoOutputPath([string]$Value) {
    if ($Value -match '^([A-Za-z]:[\\/]|/)') { return $Value }
    return ((Join-Path $RepoRoot $Value) -replace '\\', '/')
}

# BINDGEN_EXTRA_CLANG_ARGS 按空白切分，把每个 -I 的目标路径做同样的锚定
#（该变量不允许含空格路径，见 .env 说明，因此按空白切分是安全的）。
function Resolve-BindgenArgs([string]$Value) {
    $toks = $Value -split '\s+'
    for ($i = 0; $i -lt $toks.Count; $i++) {
        if ($toks[$i] -eq '-I' -and $i + 1 -lt $toks.Count) {
            $toks[$i + 1] = Resolve-RepoPath $toks[$i + 1]; $i++
        }
        elseif ($toks[$i] -match '^-I(.+)$') {
            $toks[$i] = '-I' + (Resolve-RepoPath $Matches[1])
        }
    }
    return ($toks -join ' ')
}

function Set-EnvValue([string]$Name, [string]$Value) {
    switch -Exact ($Name) {
        'LIBCLANG_PATH'            { $Value = Resolve-RepoPath $Value }
        'WDKContentRoot'           { $Value = Resolve-RepoPath $Value }
        'DEVCON_PATH'              { $Value = Resolve-RepoPath $Value }
        'WDK_BIN_PATH'             { $Value = Resolve-RepoPath $Value }
        'VMRUN_PATH'               { $Value = Resolve-RepoPath $Value }
        'VM_TEST_PACKAGE_DIR'      { $Value = Resolve-RepoOutputPath $Value }
        'VM_TEST_RESULTS_DIR'      { $Value = Resolve-RepoOutputPath $Value }
        'BINDGEN_EXTRA_CLANG_ARGS' { $Value = Resolve-BindgenArgs $Value }
    }
    Set-Item -LiteralPath "Env:$Name" -Value $Value
    return $Value
}

foreach ($entry in @(@{ File = '.env'; Force = $false },
                     @{ File = '.env.local'; Force = $true })) {
    $path = Join-Path $RepoRoot $entry.File
    $force = $entry.Force
    if (-not (Test-Path $path)) { continue }

    foreach ($raw in (Get-Content -LiteralPath $path)) {
        $line = $raw.Trim()
        if (-not $line -or $line.StartsWith('#')) { continue }

        $eq = $line.IndexOf('=')
        if ($eq -lt 1) { continue }

        $name = $line.Substring(0, $eq).Trim()
        $value = $line.Substring($eq + 1).Trim()

        # 去掉成对引号
        if ($value.Length -ge 2) {
            $first = $value[0]
            $last = $value[-1]
            if (($first -eq '"' -and $last -eq '"') -or ($first -eq "'" -and $last -eq "'")) {
                $value = $value.Substring(1, $value.Length - 2)
            }
        }

        if (-not $value) { continue }

        # .env.local（force）本身就是项目级显式配置，必须覆盖遗留的同名环境变量；
        # .env 遇到已有环境变量则跳过。
        if (-not $force -and [System.Environment]::GetEnvironmentVariable($name)) { continue }

        if ($Print) {
            $effective[$name] = (Set-EnvValue $name $value)
        }
        else {
            $null = Set-EnvValue $name $value
        }
    }
}

if ($Print) {
    foreach ($key in $effective.Keys) {
        $shown = $effective[$key]
        if ($key -eq 'VMRUN_GUEST_PASSWORD') { $shown = '***' }
        Write-Host ("{0}={1}" -f $key, $shown)
    }
}
elseif (-not $Quiet) {
    Write-Host "[load-env] 已从 .env / .env.local 加载环境变量。" -ForegroundColor Green
}

# signtool 必须压过 Git Bash / MSYS PATH 里的同名第三方程序（它会报
# "unrecognized option: sign"）。auto-detect 由 build-package.ps1 负责。
if ($env:WDK_BIN_PATH -and -not $Print) {
    if (-not (Test-Path -LiteralPath (Join-Path $env:WDK_BIN_PATH 'signtool.exe'))) {
        Write-Warning "WDK_BIN_PATH 下没有 signtool.exe：$($env:WDK_BIN_PATH)"
    }
    elseif (($env:PATH -split ';') -notcontains $env:WDK_BIN_PATH) {
        $env:PATH = "$($env:WDK_BIN_PATH);$env:PATH"
    }
}
