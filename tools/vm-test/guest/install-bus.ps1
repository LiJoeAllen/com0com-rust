<#
.SYNOPSIS
    在隔离测试虚拟机内校验签名环境并安装 com0combus 总线驱动。

.DESCRIPTION
    步骤：管理员/OS 检查 → Secure Boot 检查 → testsigning 检查（必要时开启并
    以退出码 2 要求宿主机重启）→ 导入测试证书 → devcon install/update 总线 →
    校验总线设备状态与运行中驱动哈希（可选）。

    仅限一次性/隔离的测试虚拟机使用：脚本会信任测试证书并安装测试签名驱动。
#>
param(
    [string]$BaseDir = 'C:\com0com-vmtest',

    # 本次构建 com0combus.sys 的 SHA-256；传入时校验运行中的驱动。
    [string]$ExpectedBusHash = ''
)

$ErrorActionPreference = 'Continue'
$log = Join-Path $BaseDir 'install-bus.log'
"=== install-bus $(Get-Date -Format o) ===" | Out-File $log -Encoding utf8

function L([string]$message) {
    "$(Get-Date -Format HH:mm:ss.fff) $message" | Out-File $log -Append -Encoding utf8
    Write-Host $message
}

# pnputil 输出随系统语言变化：按空行分块，匹配块内的 oemNN.inf 与 INF 名。
function Get-Com0comBusPackages {
    $out = & pnputil.exe /enum-drivers 2>&1 | Out-String
    $result = @()
    foreach ($block in ($out -split "`r?`n`r?`n")) {
        if ($block -match 'com0combus\.inf' -and $block -match '(oem\d+\.inf)') {
            $result += $Matches[1]
        }
    }
    return @($result | Select-Object -Unique)
}

# devcon findall 按硬件 ID 查找（含非 present 设备），输出首列是实例 ID。
function Get-DevconInstanceIds([string]$devconPath, [string]$hardwareId) {
    $out = & $devconPath findall $hardwareId 2>&1 | Out-String
    $ids = @()
    foreach ($line in ($out -split "`r?`n")) {
        if ($line -match '^(ROOT\\\S+|COM0COM\\\S+)') { $ids += $Matches[1].TrimEnd(':') }
    }
    return @($ids | Select-Object -Unique)
}

$exitCode = 0
try {
    $os = Get-CimInstance Win32_OperatingSystem
    L "os| $($os.Caption) build=$($os.BuildNumber) arch=$($os.OSArchitecture)"

    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $admin) { throw '需要管理员权限：请使用内置管理员账户或关闭 UAC。' }

    # Secure Boot 开启时测试签名驱动不会加载。
    $secureBoot = $null
    try { $secureBoot = Confirm-SecureBootUEFI -ErrorAction Stop } catch {
        L "secureboot| 不可用（$($_.Exception.Message)）"
    }
    if ($secureBoot -eq $true) { throw 'Secure Boot 处于开启状态，测试签名驱动无法加载。' }
    L "secureboot| $secureBoot"

    $bcd = (bcdedit /enum '{current}' 2>&1) -join "`n"
    if ($bcd -notmatch 'testsigning\s+Yes') {
        $null = bcdedit /set testsigning on 2>&1
        L 'testsigning| off → 已开启，需重启后生效'
        L 'REBOOT_REQUIRED| testsigning enabled'
        $exitCode = 2
        exit $exitCode
    }
    L 'testsigning| on'

    # 仅限隔离 VM：信任随包分发的自签测试证书。
    $cert = Join-Path $BaseDir 'WDRLocalTestCert.cer'
    if (-not (Test-Path $cert)) { throw "找不到测试证书: $cert" }
    foreach ($store in 'Root', 'TrustedPublisher') {
        $out = certutil -addstore -f $store $cert 2>&1 | Out-String
        L "cert| $store exit=$LASTEXITCODE"
        L $out.Trim()
        if ($LASTEXITCODE -ne 0) { throw "导入测试证书到 $store 失败。" }
    }

    $devcon = Join-Path $BaseDir 'devcon.exe'
    $inf = Join-Path $BaseDir 'bus\com0combus.inf'

    # 同版本驱动包会被直接复用，先移除旧设备树与旧包，确保运行的是本次二进制。
    # 根枚举总线的实例 ID 是 ROOT\SYSTEM\NNNN，不能用 COM0COM\BUS\* 匹配；
    # /deviceid 按硬件 ID 删除，/subtree 连同端口子设备一起移除。
    foreach ($hardwareId in 'COM0COM\PORT_A', 'COM0COM\PORT_B') {
        $out = & pnputil.exe /remove-device /deviceid $hardwareId /force 2>&1 | Out-String
        L "remove-device| $hardwareId exit=$LASTEXITCODE"
        L $out.Trim()
    }
    $out = & pnputil.exe /remove-device /deviceid 'COM0COM\BUS' /subtree /force 2>&1 | Out-String
    L "remove-device| COM0COM\BUS exit=$LASTEXITCODE"
    L $out.Trim()

    $pkgs = @(Get-Com0comBusPackages)
    L "packages| existing=$($pkgs -join ',')"
    foreach ($pkg in $pkgs) {
        $out = & pnputil.exe /delete-driver $pkg /uninstall /force 2>&1 | Out-String
        L "delete| $pkg exit=$LASTEXITCODE"
        L $out.Trim()
        if ($LASTEXITCODE -ne 0) { throw "删除旧总线驱动包 $pkg 失败。" }
    }

    $out = & $devcon install $inf 'COM0COM\BUS' 2>&1 | Out-String
    L "install| exit=$LASTEXITCODE"
    L $out.Trim()
    if ($LASTEXITCODE -ne 0) { throw 'devcon install 总线失败。' }

    Start-Sleep -Seconds 5
    $busIds = @(Get-DevconInstanceIds $devcon 'COM0COM\BUS')
    L "busdev| $($busIds -join ',')"
    if ($busIds.Count -ne 1) { throw "期望 1 个总线设备，实际 $($busIds.Count) 个。" }
    $dev = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -eq $busIds[0] })
    if ($dev.Count -eq 0) { throw "找不到总线设备 $($busIds[0])。" }
    L "busdev| $($dev[0].InstanceId) name=$($dev[0].FriendlyName) status=$($dev[0].Status) problem=$($dev[0].Problem)"
    if ($dev[0].Status -ne 'OK') { throw "总线设备状态异常: $($dev[0].Status)" }

    $deadline = (Get-Date).AddSeconds(30)
    $svc = $null
    do {
        $svc = Get-CimInstance Win32_SystemDriver -Filter "Name='com0combus'" -ErrorAction SilentlyContinue
        if ($svc -and $svc.State -eq 'Running') { break }
        Start-Sleep -Seconds 1
    } while ((Get-Date) -lt $deadline)
    if (-not $svc) { throw '找不到 com0combus 服务。' }
    $hash = if ($svc.PathName -and (Test-Path $svc.PathName)) {
        (Get-FileHash $svc.PathName -Algorithm SHA256).Hash
    } else { 'MISSING' }
    L "busdriver| $($svc.PathName) hash=$hash state=$($svc.State)"
    if ($svc.State -ne 'Running') { throw "com0combus 服务未运行: $($svc.State)" }
    if ($ExpectedBusHash -and $hash -ne $ExpectedBusHash) {
        L "FAIL| com0combus 哈希不匹配 expected=$ExpectedBusHash actual=$hash"
        throw '运行中的总线驱动不是本次构建，请先清理旧驱动包。'
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
