<#
.SYNOPSIS
    构建 com0com-rust 的虚拟机测试包。

.DESCRIPTION
    依次构建 setupc、com0combus（KMDF）与 com0comport（WDM），
    再把 cargo-wdk 生成的 package 目录整理为 run-vm-test.ps1 可直接分发的结构：

        <OutputDir>\bus\*.inf|sys|cat
        <OutputDir>\setupc.exe
        <OutputDir>\com0comd.exe
        <OutputDir>\sdk-smoke.exe
        <OutputDir>\capi-smoke.exe
        <OutputDir>\com0com_capi.dll
        <OutputDir>\devcon.exe
        <OutputDir>\WDRLocalTestCert.cer
        <OutputDir>\SHA256SUMS.txt

    构建环境（WDK / libclang / SDK include）由仓库根的 .env / .env.local 提供，
    脚本会自动加载；命令行显式传参仍然优先。
#>
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string]$Configuration = 'debug',

    [string]$OutputDir = '',

    # 不指定时先读 DEVCON_PATH，再在 Windows Kits\10\Tools\*\x64 下自动查找。
    [string]$DevconPath = '',

    # 内核 crate 首次 bindgen 很吃 CPU；默认 2，可由 VM_TEST_JOBS /
    # CARGO_BUILD_JOBS 覆盖。
    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'

# load-env.ps1 是 dot-source，会覆盖当前作用域的 $PSBoundParameters，
# 因此先记下哪些参数是命令行显式传入的。
$configurationExplicit = $PSBoundParameters.ContainsKey('Configuration')
$outputExplicit = $PSBoundParameters.ContainsKey('OutputDir')
$devconExplicit = $PSBoundParameters.ContainsKey('DevconPath')
$jobsExplicit = $PSBoundParameters.ContainsKey('Jobs')

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $scriptDir '..\..')).Path

# 自动加载 .env / .env.local，让新克隆的机器填完值就能直接跑。
$envLoader = Join-Path $repo 'tools\load-env.ps1'
if (Test-Path -LiteralPath $envLoader) { . $envLoader -Quiet }

if (-not $configurationExplicit -and $env:VM_TEST_CONFIGURATION) {
    $Configuration = $env:VM_TEST_CONFIGURATION
}
if ($Configuration -notin @('debug', 'release')) {
    throw "无效的构建配置：$Configuration（只支持 debug / release）"
}

if (-not $outputExplicit -and $env:VM_TEST_PACKAGE_DIR) { $OutputDir = $env:VM_TEST_PACKAGE_DIR }
if (-not $OutputDir) { $OutputDir = Join-Path $repo 'target\vm-test-package' }
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)

if (-not $devconExplicit -and $env:DEVCON_PATH) { $DevconPath = $env:DEVCON_PATH }

if (-not $jobsExplicit) {
    if ($env:VM_TEST_JOBS) { $Jobs = [int]$env:VM_TEST_JOBS }
    elseif ($env:CARGO_BUILD_JOBS) { $Jobs = [int]$env:CARGO_BUILD_JOBS }
}
if ($Jobs -lt 1) { throw "Jobs 必须是正整数：$Jobs" }

function Step([string]$message) {
    Write-Host "==> $message" -ForegroundColor Cyan
}

function Invoke-Checked([string]$exe, [string[]]$arguments, [string]$workDir) {
    Push-Location $workDir
    try {
        & $exe @arguments
        $code = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }
    if ($code -ne 0) {
        throw "$exe $($arguments -join ' ') 失败（exit $code）"
    }
}

function Find-Devcon {
    if ($DevconPath) {
        if (-not (Test-Path $DevconPath)) { throw "找不到 devcon.exe: $DevconPath" }
        return (Resolve-Path $DevconPath).Path
    }

    $roots = @()
    if ($env:WDKContentRoot) { $roots += $env:WDKContentRoot }
    if (${env:ProgramFiles(x86)}) { $roots += (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10') }
    if ($env:ProgramFiles) { $roots += (Join-Path $env:ProgramFiles 'Windows Kits\10') }
    # WDK 可能装在其他盘（WDKContentRoot 未设置时）。
    foreach ($drive in @(Get-PSDrive -PSProvider FileSystem -ErrorAction SilentlyContinue)) {
        $roots += (Join-Path $drive.Root 'Windows Kits\10')
    }
    $roots = @($roots | Select-Object -Unique)

    foreach ($root in $roots) {
        $tools = Join-Path $root 'Tools'
        if (-not (Test-Path $tools)) { continue }
        $found = Get-ChildItem -Path $tools -Recurse -Filter 'devcon.exe' -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\' } |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    throw '未找到 devcon.exe。请通过 -DevconPath 指定 WDK Tools 下的 x64\devcon.exe。'
}

function Copy-DriverPackage([string]$source, [string]$destination) {
    if (-not (Test-Path $source)) {
        throw "找不到 cargo-wdk 产物目录: $source（请先用 cargo wdk build 构建 $Configuration 配置）"
    }
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    foreach ($pattern in '*.inf', '*.sys', '*.cat') {
        $files = @(Get-ChildItem -Path $source -Filter $pattern -File -ErrorAction SilentlyContinue)
        if ($files.Count -eq 0) { throw "$source 下缺少 $pattern" }
        $files | Copy-Item -Destination $destination -Force
    }
}

Step "构建配置: $Configuration（CARGO_BUILD_JOBS=$Jobs）"
$env:CARGO_BUILD_JOBS = "$Jobs"

# Git Bash/MSYS 环境的 PATH 里常带有同名的第三方 signtool（如 msys64\mingw64\bin），
# 会遮蔽 WDK/SDK 的 signtool 并让 cargo-wdk 签名失败。优先使用 WDK_BIN_PATH；
# 未指定时选**含有 signtool.exe** 的最新版本目录前置（signtool 只随 Windows SDK
# 的版本化 bin 目录发布，WDK 自身的 bin\<ver> 不含它）。
$signtoolDir = ''
if ($env:WDK_BIN_PATH) {
    if (Test-Path -LiteralPath (Join-Path $env:WDK_BIN_PATH 'signtool.exe')) {
        $signtoolDir = $env:WDK_BIN_PATH
    }
    else {
        Write-Warning "WDK_BIN_PATH 下没有 signtool.exe：$($env:WDK_BIN_PATH)"
    }
}
if (-not $signtoolDir -and $env:WDKContentRoot) {
    $versionedBins = Get-ChildItem -Path (Join-Path $env:WDKContentRoot 'bin') -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+)+' } |
        Sort-Object { [version]$_.Name } -Descending
    foreach ($bin in $versionedBins) {
        $dir = Join-Path $bin.FullName 'x64'
        if ((Test-Path $dir) -and (Test-Path (Join-Path $dir 'signtool.exe'))) {
            $signtoolDir = $dir
            break
        }
    }
}
if ($signtoolDir -and (($env:PATH -split ';') -notcontains $signtoolDir)) {
    $env:PATH = "$signtoolDir;$env:PATH"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw '找不到 cargo，请先安装 Rust 工具链。'
}

$releaseArg = @()
if ($Configuration -eq 'release') { $releaseArg = @('--release') }

Step '构建 setupc（根 workspace）'
Invoke-Checked 'cargo' (@('build', '-p', 'com0com-setupc') + $releaseArg) $repo

Step '构建 SDK/daemon 工具（根 workspace）'
Invoke-Checked 'cargo' (@('build', '-p', 'com0com-daemon', '-p', 'com0com-sdk-smoke', '-p', 'com0com-capi-smoke') + $releaseArg) $repo

# 单独构建 C ABI 动态库：上面的命令会为冒烟测试启用 fake feature，
# 这里不带 feature 重建，保证分发的 DLL 只有真实后端。
Step '构建 C ABI 动态库（根 workspace）'
Invoke-Checked 'cargo' (@('build', '-p', 'com0com-capi') + $releaseArg) $repo

Step '构建 com0combus（KMDF，独立 workspace）'
Invoke-Checked 'cargo' (@('wdk', 'build') + $releaseArg) (Join-Path $repo 'crates\com0combus')

Step '构建 com0comport（WDM，独立 workspace）'
Invoke-Checked 'cargo' (@('wdk', 'build') + $releaseArg) (Join-Path $repo 'crates\com0comport')

Step "整理测试包: $OutputDir"
if (Test-Path $OutputDir) {
    Remove-Item -Path $OutputDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$busPackage = Join-Path $repo "crates\com0combus\target\$Configuration\com0combus_package"
$portPackage = Join-Path $repo "crates\com0comport\target\$Configuration\com0comport_package"
Copy-DriverPackage $busPackage (Join-Path $OutputDir 'bus')
Copy-DriverPackage $portPackage (Join-Path $OutputDir 'bus')

$setupc = Join-Path $repo "target\$Configuration\setupc.exe"
if (-not (Test-Path $setupc)) { throw "找不到 setupc 构建产物: $setupc" }
Copy-Item $setupc (Join-Path $OutputDir 'setupc.exe') -Force

$binaries = @('com0comd.exe', 'sdk-smoke.exe', 'capi-smoke.exe', 'com0com_capi.dll')
foreach ($name in $binaries) {
    $path = Join-Path $repo "target\$Configuration\$name"
    if (-not (Test-Path $path)) { throw "找不到构建产物: $path" }
    Copy-Item $path (Join-Path $OutputDir $name) -Force
}

$devcon = Find-Devcon
Copy-Item $devcon (Join-Path $OutputDir 'devcon.exe') -Force

$cert = Get-ChildItem -Path $busPackage -Filter '*.cer' -File -ErrorAction SilentlyContinue | Select-Object -First 1
if ($cert) {
    Copy-Item $cert.FullName (Join-Path $OutputDir 'WDRLocalTestCert.cer') -Force
}
else {
    Write-Warning '驱动包里没有 .cer；客户机必须已手动信任测试证书，否则驱动无法加载。'
}

# 记录哈希：run-vm-test.ps1 用它确认客户机加载的确实是本次构建。
$sums = foreach ($file in (Get-ChildItem -Path $OutputDir -Recurse -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' })) {
    $relative = $file.FullName.Substring($OutputDir.Length + 1).Replace('\', '/')
    "$((Get-FileHash $file.FullName -Algorithm SHA256).Hash)  $relative"
}
$sums | Set-Content -Path (Join-Path $OutputDir 'SHA256SUMS.txt') -Encoding ascii

Step '完成'
Write-Host "测试包: $OutputDir"
Get-ChildItem -Path $OutputDir -Recurse -File | ForEach-Object {
    Write-Host ("  {0}  {1,10} B" -f $_.FullName.Substring($OutputDir.Length + 1), $_.Length)
}
