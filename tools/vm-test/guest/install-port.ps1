<#
.SYNOPSIS
    在隔离测试虚拟机内替换 com0comport 驱动包。

.DESCRIPTION
    DriverStore 里存在同版本旧包时，pnputil /add-driver 会直接复用旧二进制。
    因此先删除所有 com0comport 包（/uninstall /force），再添加本次构建的新包，
    并校验 DriverStore 中确实存在本次构建的 com0comport.sys 哈希。

    仅操作 com0comport 包，不删除 com0combus 总线包。
#>
param(
    [string]$BaseDir = 'C:\com0com-vmtest',

    # 本次构建 com0comport.sys 的 SHA-256；传入时校验 DriverStore 中的副本。
    [string]$ExpectedPortHash = ''
)

$ErrorActionPreference = 'Continue'
$log = Join-Path $BaseDir 'install-port.log'
"=== install-port $(Get-Date -Format o) ===" | Out-File $log -Encoding utf8

function L([string]$message) {
    "$(Get-Date -Format HH:mm:ss.fff) $message" | Out-File $log -Append -Encoding utf8
    Write-Host $message
}

# pnputil 输出随系统语言变化：按空行分块，匹配块内的 oemNN.inf 与 INF 名。
function Get-Com0comPortPackages {
    $out = & pnputil.exe /enum-drivers 2>&1 | Out-String
    $result = @()
    foreach ($block in ($out -split "`r?`n`r?`n")) {
        if ($block -match 'com0comport\.inf' -and $block -match '(oem\d+\.inf)') {
            $result += $Matches[1]
        }
    }
    return @($result | Select-Object -Unique)
}

$exitCode = 0
try {
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $admin) { throw '需要管理员权限：请使用内置管理员账户或关闭 UAC。' }

    $pkgs = @(Get-Com0comPortPackages)
    L "packages| existing=$($pkgs -join ',')"
    foreach ($pkg in $pkgs) {
        $out = & pnputil.exe /delete-driver $pkg /uninstall /force 2>&1 | Out-String
        L "delete| $pkg exit=$LASTEXITCODE"
        L $out.Trim()
        if ($LASTEXITCODE -ne 0) { throw "删除旧驱动包 $pkg 失败。" }
    }

    $inf = Join-Path $BaseDir 'bus\com0comport.inf'
    $out = & pnputil.exe /add-driver $inf /install 2>&1 | Out-String
    L "add-driver| exit=$LASTEXITCODE"
    L $out.Trim()
    if ($LASTEXITCODE -ne 0) { throw '添加 com0comport 驱动包失败。' }

    $remaining = @(Get-Com0comPortPackages)
    L "packages| remaining=$($remaining -join ',')"
    if ($remaining.Count -eq 0) { throw 'DriverStore 中没有 com0comport 包。' }

    $storeHashes = @()
    $repo = Join-Path $env:WINDIR 'System32\DriverStore\FileRepository'
    foreach ($dir in @(Get-ChildItem $repo -Directory -Filter 'com0comport.inf_amd64_*' -ErrorAction SilentlyContinue)) {
        $sys = Join-Path $dir.FullName 'com0comport.sys'
        $hash = if (Test-Path $sys) { (Get-FileHash $sys -Algorithm SHA256).Hash } else { 'MISSING' }
        L "store| $($dir.Name) syshash=$hash"
        $storeHashes += $hash
    }
    if ($ExpectedPortHash -and ($storeHashes -notcontains $ExpectedPortHash)) {
        L "FAIL| DriverStore 中没有本次构建的 com0comport.sys（expected=$ExpectedPortHash）"
        throw '旧驱动包未被完全替换。'
    }
    L 'RESULT| PASS'
}
catch {
    L "ERROR| $($_.Exception.Message)"
    L 'RESULT| FAIL'
    $exitCode = 1
}
finally {
    "=== done (exit $exitCode) ===" | Out-File $log -Append -Encoding utf8
}
exit $exitCode
