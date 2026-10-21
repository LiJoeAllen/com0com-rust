<#
.SYNOPSIS
    构建 com0com-rust 的虚拟机测试包。

.DESCRIPTION
    依次构建 setupc、com0combus（KMDF）与 com0comport（WDM），
    再把 cargo-wdk 生成的 package 目录整理为 run-vm-test.ps1 可直接分发的结构：

        <OutputDir>\bus\*.inf|sys|cat
        <OutputDir>\setupc.exe
        <OutputDir>\devcon.exe
        <OutputDir>\WDRLocalTestCert.cer
        <OutputDir>\SHA256SUMS.txt

    构建环境（WDK / libclang / SDK include）按仓库根 README 配置；
    脚本只负责设一个安全的并行度，避免低配机编译内核 crate 时卡死。
#>
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string]$Configuration = 'debug',

    [string]$OutputDir = '',

    # 不指定时在 Windows Kits\10\Tools\*\x64 下自动查找。
    [string]$DevconPath = '',

    # 内核 crate 首次 bindgen 很吃 CPU；固定小并行度。
    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $scriptDir '..\..')).Path
if (-not $OutputDir) { $OutputDir = Join-Path $repo 'target\vm-test-package' }
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)

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

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw '找不到 cargo，请先安装 Rust 工具链。'
}

$releaseArg = @()
if ($Configuration -eq 'release') { $releaseArg = @('--release') }

Step '构建 setupc（根 workspace）'
Invoke-Checked 'cargo' (@('build', '-p', 'com0com-setupc') + $releaseArg) $repo

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
