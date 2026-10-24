<#
.SYNOPSIS
    在 VMware 隔离测试虚拟机中执行 com0com-rust 驱动的端到端验证。

.DESCRIPTION
    流程：构建测试包（可选）→ 回滚快照/启动 VM → 等待 VMware Tools 与
    PowerShell 就绪 → 复制测试包与客户机脚本 → 安装总线 → 替换端口驱动包
    → 重启 → 运行串口对检查 → 收集日志并解析 `RESULT|`。

    凭据参数（-gu/-gp）由本脚本放在 vmrun 子命令之前；runProgramInGuest 没有
    stdout，因此所有结果都写入客户机日志后拷回。

    仅限隔离的一次性测试虚拟机：客户机会信任测试证书并启用 testsigning。
#>
[CmdletBinding()]
param(
    # 以下三个连接参数可只写在 .env.local，命令行显式传参优先。
    [string]$VmxPath = '',

    [string]$GuestUser = '',

    [string]$GuestPassword = '',

    # 指定时先关机回滚快照再启动；快照必须处于关机状态。
    [string]$SnapshotName = '',

    [ValidateSet('debug', 'release')]
    [string]$Configuration = 'debug',

    [string]$PackageDir = '',

    [string]$ResultsDir = '',

    [string]$GuestDir = 'C:\com0com-vmtest',

    [string]$VmrunPath = '',

    [switch]$SkipBuild,

    [switch]$SkipReboot,

    # 冷启动 / 快照回滚后的首次启动可能超过 5 分钟，实测繁忙主机可超过 10 分钟。
    [int]$BootTimeoutSec = 1200,

    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'

# load-env.ps1 是 dot-source，会覆盖当前作用域的 $PSBoundParameters，
# 因此先记下哪些参数是命令行显式传入的。
$explicit = @{}
foreach ($name in 'VmxPath', 'GuestUser', 'GuestPassword', 'SnapshotName', 'Configuration',
    'PackageDir', 'ResultsDir', 'GuestDir', 'VmrunPath', 'BootTimeoutSec', 'Jobs') {
    $explicit[$name] = $PSBoundParameters.ContainsKey($name)
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

# 自动加载 .env / .env.local，让新克隆的机器填完值就能直接跑。
$envLoader = Join-Path $repo 'tools\load-env.ps1'
if (Test-Path -LiteralPath $envLoader) { . $envLoader -Quiet }

# 优先级：命令行显式传参 > .env.local > 已有环境变量 > .env > 脚本默认值。
if (-not $explicit.VmxPath        -and $env:VMRUN_VMX_PATH)        { $VmxPath        = $env:VMRUN_VMX_PATH }
if (-not $explicit.GuestUser      -and $env:VMRUN_GUEST_USER)      { $GuestUser      = $env:VMRUN_GUEST_USER }
if (-not $explicit.GuestPassword  -and $env:VMRUN_GUEST_PASSWORD)  { $GuestPassword  = $env:VMRUN_GUEST_PASSWORD }
if (-not $explicit.SnapshotName   -and $env:VMRUN_SNAPSHOT_NAME)   { $SnapshotName   = $env:VMRUN_SNAPSHOT_NAME }
if (-not $explicit.VmrunPath      -and $env:VMRUN_PATH)            { $VmrunPath      = $env:VMRUN_PATH }
if (-not $explicit.PackageDir     -and $env:VM_TEST_PACKAGE_DIR)   { $PackageDir     = $env:VM_TEST_PACKAGE_DIR }
if (-not $explicit.ResultsDir     -and $env:VM_TEST_RESULTS_DIR)   { $ResultsDir     = $env:VM_TEST_RESULTS_DIR }
if (-not $explicit.GuestDir       -and $env:VM_TEST_GUEST_DIR)     { $GuestDir       = $env:VM_TEST_GUEST_DIR }
if (-not $explicit.Configuration  -and $env:VM_TEST_CONFIGURATION) { $Configuration  = $env:VM_TEST_CONFIGURATION }
if (-not $explicit.BootTimeoutSec -and $env:VM_TEST_BOOT_TIMEOUT_SEC) {
    $BootTimeoutSec = [int]$env:VM_TEST_BOOT_TIMEOUT_SEC
}
if (-not $explicit.Jobs) {
    if ($env:VM_TEST_JOBS) { $Jobs = [int]$env:VM_TEST_JOBS }
    elseif ($env:CARGO_BUILD_JOBS) { $Jobs = [int]$env:CARGO_BUILD_JOBS }
}

if (-not $VmxPath)       { throw '缺少测试虚拟机：请传 -VmxPath，或在 .env.local 中设置 VMRUN_VMX_PATH。' }
if (-not $GuestUser)     { throw '缺少客户机账号：请传 -GuestUser，或在 .env.local 中设置 VMRUN_GUEST_USER。' }
if (-not $GuestPassword) { throw '缺少客户机口令：请传 -GuestPassword，或在 .env.local 中设置 VMRUN_GUEST_PASSWORD。' }
if ($Configuration -notin @('debug', 'release')) {
    throw "无效的构建配置：$Configuration（只支持 debug / release）"
}
if ($BootTimeoutSec -lt 1) { throw "BootTimeoutSec 必须是正整数：$BootTimeoutSec" }
if ($Jobs -lt 1) { throw "Jobs 必须是正整数：$Jobs" }

$GuestPowerShell = 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'

if (-not (Test-Path $VmxPath)) { throw "找不到虚拟机: $VmxPath" }
$VmxPath = (Resolve-Path $VmxPath).Path

if (-not $PackageDir) { $PackageDir = Join-Path $repo "target\vm-test-package" }
$PackageDir = [System.IO.Path]::GetFullPath($PackageDir)

if (-not $ResultsDir) {
    $ResultsDir = Join-Path $repo ("target\vm-test-results\" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
$ResultsDir = [System.IO.Path]::GetFullPath($ResultsDir)
New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null

function Find-Vmrun {
    if ($VmrunPath) {
        if (-not (Test-Path $VmrunPath)) { throw "找不到 vmrun.exe: $VmrunPath" }
        return (Resolve-Path $VmrunPath).Path
    }
    $command = Get-Command vmrun.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $roots = @()
    if (${env:ProgramFiles(x86)}) { $roots += ${env:ProgramFiles(x86)} }
    if ($env:ProgramFiles) { $roots += $env:ProgramFiles }
    foreach ($root in $roots) {
        $candidate = Join-Path $root 'VMware\VMware Workstation\vmrun.exe'
        if (Test-Path $candidate) { return $candidate }
    }
    foreach ($key in 'HKLM:\SOFTWARE\WOW6432Node\VMware, Inc.\VMware Workstation',
        'HKLM:\SOFTWARE\VMware, Inc.\VMware Workstation') {
        $installPath = (Get-ItemProperty $key -ErrorAction SilentlyContinue).InstallPath
        if ($installPath) {
            $candidate = Join-Path $installPath 'vmrun.exe'
            if (Test-Path $candidate) { return $candidate }
        }
    }
    # Workstation 可能装在非系统盘。
    foreach ($drive in @(Get-PSDrive -PSProvider FileSystem -ErrorAction SilentlyContinue)) {
        foreach ($programFiles in 'Program Files', 'Program Files (x86)') {
            $candidate = Join-Path $drive.Root "$programFiles\VMware\VMware Workstation\vmrun.exe"
            if (Test-Path $candidate) { return $candidate }
        }
    }
    throw '未找到 vmrun.exe，请通过 -VmrunPath 指定 VMware Workstation 安装目录下的 vmrun.exe。'
}

function Assert-SnapshotPoweredOff([string]$Name) {
    # VMware 允许给运行中的 VM 建快照；这类快照带内存状态，回滚后客户机可能
    # 停在锁屏且 VMware Tools 起不来（实测 type=2）。只接受关机快照（type=0）。
    # 快照元数据写在同名的 .vmsd 里，不在 .vmx。
    $vmsd = [System.IO.Path]::ChangeExtension($VmxPath, '.vmsd')
    if (-not (Test-Path -LiteralPath $vmsd)) { return }
    $names = @{}
    $types = @{}
    foreach ($line in Get-Content -LiteralPath $vmsd) {
        if ($line -match '^snapshot(\d+)\.displayName = "(.*)"$') {
            $names[$matches[2]] = $matches[1]
        }
        elseif ($line -match '^snapshot(\d+)\.type = "(.*)"$') {
            $types[$matches[1]] = $matches[2]
        }
    }
    # 快照不存在时交给 vmrun 报错，这里不预判。
    if (-not $names.ContainsKey($Name)) { return }
    $type = $types[$names[$Name]]
    if ($type -and $type -ne '0') {
        throw "快照 '$Name' 不是关机快照（type=$type）；请关机后重建快照，或改用关机快照。"
    }
}

$vmrun = Find-Vmrun
$auth = @('-gu', $GuestUser, '-gp', $GuestPassword)

function Test-VmRunning {
    $list = & $vmrun -T ws list 2>$null
    return [bool](@($list) | Where-Object { $_ -and $_.Trim() -ieq $VmxPath })
}

function Invoke-Vmrun([string[]]$Arguments) {
    $out = & $vmrun -T ws @Arguments 2>&1
    $code = $LASTEXITCODE
    if ($out) { foreach ($line in @($out)) { Write-Verbose $line } }
    if ($code -ne 0) { throw "vmrun 失败（exit $code）: $($Arguments -join ' ')" }
}

function Wait-GuestReady([int]$TimeoutSec, [int]$InitialDelaySec = 0) {
    if ($InitialDelaySec -gt 0) { Start-Sleep -Seconds $InitialDelaySec }
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $state = ''
        try { $state = (& $vmrun -T ws checkToolsState $VmxPath 2>$null | Out-String).Trim() } catch {}
        if ($state -match 'running') {
            # cmd.exe 的探测返回值不可靠，改用 PowerShell 的显式退出码。
            $null = & $vmrun -T ws @auth runProgramInGuest $VmxPath $GuestPowerShell -NoProfile -Command 'exit 0' 2>&1
            if ($LASTEXITCODE -eq 0) { return $true }
        }
        Start-Sleep -Seconds 5
    }
    return $false
}

function Restart-Guest {
    Write-Host '==> 重启客户机'
    $null = & $vmrun -T ws @auth runProgramInGuest $VmxPath -noWait `
        C:\Windows\System32\shutdown.exe /r /t 5 /f 2>&1
    Start-Sleep -Seconds 20
    if (-not (Wait-GuestReady -TimeoutSec $BootTimeoutSec -InitialDelaySec 10)) {
        throw '客户机重启后未就绪。'
    }
}

function New-GuestDirectory([string]$path) {
    # 目录已存在时 vmrun 会报错，忽略即可。
    $null = & $vmrun -T ws @auth createDirectoryInGuest $VmxPath $path 2>&1
}

function Copy-ToGuest([string]$hostPath, [string]$guestPath) {
    $out = & $vmrun -T ws @auth copyFileFromHostToGuest $VmxPath $hostPath $guestPath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "复制到客户机失败: $hostPath -> $guestPath`n$($out -join "`n")"
    }
}

function Copy-FromGuest([string]$guestPath, [string]$hostPath) {
    $out = & $vmrun -T ws @auth copyFileFromGuestToHost $VmxPath $guestPath $hostPath 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "取回日志失败: $guestPath`n$($out -join "`n")"
        return $false
    }
    return $true
}

function Invoke-GuestScript([string]$Name, [hashtable]$Arguments = @{}) {
    $path = "$GuestDir\guest\$Name"
    $argv = @($GuestPowerShell, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $path)
    foreach ($key in $Arguments.Keys) {
        $argv += "-$key"
        $argv += [string]$Arguments[$key]
    }
    Write-Host "==> 客户机脚本: $Name"
    $out = & $vmrun -T ws @auth runProgramInGuest $VmxPath @argv 2>&1
    $code = $LASTEXITCODE
    if ($out) { foreach ($line in @($out)) { Write-Host "    $line" } }
    Write-Host "    退出码: $code"
    return $code
}

function Get-PackageHash([string]$relative) {
    $sums = Join-Path $PackageDir 'SHA256SUMS.txt'
    if (-not (Test-Path $sums)) { throw "缺少 SHA256SUMS.txt: $sums" }
    $line = Get-Content $sums | Where-Object { (($_ -split '\s+')[1]) -eq $relative } | Select-Object -First 1
    if (-not $line) { throw "SHA256SUMS.txt 中没有 $relative" }
    return ($line -split '\s+')[0]
}

$status = 'FAIL'
# 只有真正在本次运行中执行过 check-pairs.ps1 才允许判定 PASS；否则客户机里
# 上一次遗留的 check-pairs.log 会被当成本次结果（曾导致就绪超时仍报 PASS）。
$checkRan = $false
$checkCode = 1
try {
    if (-not $SkipBuild) {
        Write-Host "==> 构建测试包（$Configuration）"
        & (Join-Path $PSScriptRoot 'build-package.ps1') -Configuration $Configuration `
            -OutputDir $PackageDir -Jobs $Jobs
    }
    if (-not (Test-Path (Join-Path $PackageDir 'bus\com0combus.inf'))) {
        throw "测试包不完整: $PackageDir（先运行 build-package.ps1）"
    }
    $expectedBusHash = Get-PackageHash 'bus/com0combus.sys'
    $expectedPortHash = Get-PackageHash 'bus/com0comport.sys'
    Write-Host "==> 期望哈希 bus=$expectedBusHash port=$expectedPortHash"

    if ($SnapshotName) {
        Assert-SnapshotPoweredOff $SnapshotName
        if (Test-VmRunning) {
            Write-Host '==> 停止运行中的测试虚拟机（hard）'
            Invoke-Vmrun @('stop', $VmxPath, 'hard')
            Start-Sleep -Seconds 5
        }
        Write-Host "==> 回滚快照: $SnapshotName"
        Invoke-Vmrun @('revertToSnapshot', $VmxPath, $SnapshotName)
    }
    if (-not (Test-VmRunning)) {
        Write-Host '==> 启动虚拟机'
        Invoke-Vmrun @('start', $VmxPath, 'nogui')
    }
    if (-not (Wait-GuestReady -TimeoutSec $BootTimeoutSec -InitialDelaySec 10)) {
        throw '客户机未就绪（VMware Tools 或 PowerShell 不可用）。'
    }

    Write-Host "==> 复制测试包到 $GuestDir"
    New-GuestDirectory $GuestDir
    New-GuestDirectory "$GuestDir\bus"
    New-GuestDirectory "$GuestDir\guest"
    foreach ($file in @(Get-ChildItem -Path $PackageDir -File)) {
        Copy-ToGuest $file.FullName "$GuestDir\$($file.Name)"
    }
    foreach ($file in @(Get-ChildItem -Path (Join-Path $PackageDir 'bus') -File)) {
        Copy-ToGuest $file.FullName "$GuestDir\bus\$($file.Name)"
    }
    foreach ($file in @(Get-ChildItem -Path (Join-Path $PSScriptRoot 'guest') -File)) {
        Copy-ToGuest $file.FullName "$GuestDir\guest\$($file.Name)"
    }

    $busArgs = @{ BaseDir = $GuestDir; ExpectedBusHash = $expectedBusHash }
    $code = Invoke-GuestScript 'install-bus.ps1' $busArgs
    if ($code -eq 2) {
        # 刚开启 testsigning，需要重启后再安装。
        Restart-Guest
        $code = Invoke-GuestScript 'install-bus.ps1' $busArgs
    }
    if ($code -ne 0) { throw 'install-bus.ps1 失败（详见 install-bus.log）。' }

    $portArgs = @{ BaseDir = $GuestDir; ExpectedPortHash = $expectedPortHash }
    $code = Invoke-GuestScript 'install-port.ps1' $portArgs
    if ($code -ne 0) { throw 'install-port.ps1 失败（详见 install-port.log）。' }

    if (-not $SkipReboot) { Restart-Guest }

    $checkArgs = @{
        BaseDir          = $GuestDir
        ExpectedBusHash  = $expectedBusHash
        ExpectedPortHash = $expectedPortHash
    }
    $checkRan = $true
    $checkCode = Invoke-GuestScript 'check-pairs.ps1' $checkArgs
    if ($checkCode -ne 0) { Write-Host '串口对检查返回失败。' -ForegroundColor Red }
}
catch {
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
}
finally {
    Write-Host '==> 收集客户机日志'
    foreach ($name in 'install-bus.log', 'install-port.log', 'check-pairs.log', 'com0comd.log') {
        $null = Copy-FromGuest "$GuestDir\$name" (Join-Path $ResultsDir $name)
    }

    $checkLog = Join-Path $ResultsDir 'check-pairs.log'
    Write-Host ''
    if (-not $checkRan) {
        Write-Host '未执行 check-pairs.ps1，结果为 FAIL（不解析客户机遗留日志）。'
    }
    elseif (Test-Path $checkLog) {
        $resultLine = Select-String -Path $checkLog -Pattern 'RESULT\|' | Select-Object -Last 1
        if ($resultLine) {
            Write-Host $resultLine.Line
            if ($checkCode -eq 0 -and $resultLine.Line -match 'RESULT\|\s*PASS') { $status = 'PASS' }
        }
        # 行首带时间戳，按子串匹配；排除结尾的 RESULT| FAIL 汇总行。
        $failures = @(Select-String -Path $checkLog -Pattern 'FAIL\|' |
            Where-Object { $_.Line -notmatch 'RESULT\|' })
        if ($failures.Count -gt 0) {
            Write-Host '--- 失败项 ---'
            $failures | ForEach-Object { Write-Host $_.Line }
        }
    }
    else {
        Write-Host '未取回 check-pairs.log。'
    }
    Write-Host "结果: $status"
    Write-Host "日志目录: $ResultsDir"
}

if ($status -eq 'PASS') { exit 0 }
exit 1
