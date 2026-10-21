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
    [switch]$Print
)

$ErrorActionPreference = 'Stop'

if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

$effective = [ordered]@{}

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
            $effective[$name] = $value
        }
        else {
            Set-Item -LiteralPath "Env:$name" -Value $value
        }
    }
}

if ($Print) {
    foreach ($key in $effective.Keys) {
        Write-Host ("{0}={1}" -f $key, $effective[$key])
    }
}
else {
    Write-Host "[load-env] 已从 .env / .env.local 加载环境变量。" -ForegroundColor Green
}
