<#
.SYNOPSIS
    在隔离测试虚拟机内验证 com0com 串口对的完整生命周期。

.DESCRIPTION
    覆盖：清理旧 pair → 显式命名 pair 0（COM15/COM16）→ 设备状态 → 运行中
    驱动哈希 → 双向 4096 字节传输 → 删除后端口/PHANTOM/SERIALCOMM 清理 →
    pair 5（COM20/COM21）双向 1024 字节 + 波特率仿真写路径 → 自动命名 pair 7 →
    SDK/C ABI 冒烟 → daemon JSON-RPC → --autosave/--restore → 最终列表清空。

    每条断言写 `PASS|` / `FAIL|` 到 check-pairs.log，结尾写
    `RESULT| PASS|FAIL checks=N failures=M`，供宿主机解析。
#>
param(
    [string]$BaseDir = 'C:\com0com-vmtest',
    [string]$ExpectedBusHash = '',
    [string]$ExpectedPortHash = '',
    [string]$PairAPort = 'COM15',
    [string]$PairBPort = 'COM16',
    [string]$PairCPort = 'COM20',
    [string]$PairDPort = 'COM21',
    [int]$PairTimeoutSec = 60
)

$ErrorActionPreference = 'Continue'
$log = Join-Path $BaseDir 'check-pairs.log'
$setupc = Join-Path $BaseDir 'setupc.exe'
"=== check-pairs $(Get-Date -Format o) ===" | Out-File $log -Encoding utf8

$script:checks = 0
$script:failures = 0
$script:setupcExit = 0

function L([string]$message) {
    "$(Get-Date -Format HH:mm:ss.fff) $message" | Out-File $log -Append -Encoding utf8
    Write-Host $message
}

function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $script:checks++
    $suffix = if ($detail) { " | $detail" } else { '' }
    if ($ok) {
        L "PASS| $name$suffix"
    }
    else {
        $script:failures++
        L "FAIL| $name$suffix"
    }
}

function Brief([string]$text) {
    if (-not $text) { return '' }
    return ((($text -split "`r?`n") | Where-Object { $_.Trim() } | Select-Object -First 2) -join ' / ')
}

function RunSetupc([string]$tag, [string[]]$arguments) {
    $result = Run-Tool $tag $setupc $arguments 60000
    $script:setupcExit = $result.ExitCode
    return $result
}

function Run-Tool([string]$tag, [string]$exe, [string[]]$arguments, [int]$timeoutMs = 180000) {
    if (-not (Test-Path $exe)) {
        L "$tag| MISSING $exe"
        return [pscustomobject]@{ ExitCode = -1; Text = '' }
    }

    # Start-Process + 重定向时 ExitCode 可能为空，改用 .NET Process 异步读取输出。
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.Arguments = (($arguments | ForEach-Object {
        if ($_ -match '\s') { '"' + $_ + '"' } else { $_ }
    }) -join ' ')
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $psi
    try { $null = $process.Start() } catch {
        L "$tag| START ERROR $($_.Exception.Message)"
        return [pscustomobject]@{ ExitCode = -1; Text = '' }
    }

    $outTask = $process.StandardOutput.ReadToEndAsync()
    $errTask = $process.StandardError.ReadToEndAsync()
    $finished = $process.WaitForExit($timeoutMs)
    if (-not $finished) {
        try { $process.Kill() } catch {}
        L "$tag| TIMEOUT"
        return [pscustomobject]@{ ExitCode = -1; Text = '' }
    }
    $text = [string]$outTask.Result + [string]$errTask.Result
    $code = $process.ExitCode
    $process.Dispose()
    L "$tag| exit=$code"
    foreach ($line in ($text -split "`r?`n")) { if ($line.Trim()) { L "$tag> $line" } }
    return [pscustomobject]@{ ExitCode = $code; Text = $text }
}

function Start-Daemon([string]$exe, [string]$pipe, [string]$logPath, [string]$statePath, [bool]$restore) {
    $arguments = @('run', '--pipe', $pipe, '--autosave', '--log', $logPath)
    if ($restore) { $arguments += '--restore' }
    $argList = (($arguments | ForEach-Object {
        if ($_ -match '\s') { '"' + $_ + '"' } else { $_ }
    }) -join ' ')

    # daemon 通过 COM0COM_STATE 指向测试自己的状态文件，避免污染系统默认路径。
    $previous = $env:COM0COM_STATE
    $env:COM0COM_STATE = $statePath
    try {
        return Start-Process -FilePath $exe -ArgumentList $argList -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput "$logPath.stdout" -RedirectStandardError "$logPath.stderr"
    }
    finally {
        $env:COM0COM_STATE = $previous
    }
}

function Wait-DaemonStatus([string]$exe, [string]$pipe, [int]$timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    $result = $null
    do {
        $result = Run-Tool 'daemon-status' $exe @('status', '--pipe', $pipe) 15000
        if ($result.ExitCode -eq 0) { return $result }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    return $result
}

function Stop-DaemonProcess($process) {
    if ($process -and -not $process.HasExited) {
        try {
            $process.Kill()
            $null = $process.WaitForExit(5000)
        }
        catch {}
    }
}

function Get-PairIds {
    $result = RunSetupc 'list' @('list')
    $ids = @()
    foreach ($line in ($result.Text -split "`r?`n")) {
        if ($line -match '^\s*(\d+)\s+') { $ids += [int]$Matches[1] }
    }
    return @($ids | Sort-Object -Unique)
}

function Get-Ports {
    try { return @([System.IO.Ports.SerialPort]::GetPortNames() | Sort-Object) }
    catch { L "ports| ERROR $($_.Exception.Message)"; return @() }
}

function Wait-ForPorts([string[]]$names, [bool]$present, [int]$timeoutSec) {
    if (-not $names -or $names.Count -eq 0) { return $false }
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    do {
        $current = @(Get-Ports)
        $ok = $true
        foreach ($name in $names) {
            if ($present -and ($current -notcontains $name)) { $ok = $false }
            if (-not $present -and ($current -contains $name)) { $ok = $false }
        }
        if ($ok) { return $true }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    return $false
}

function Get-PairDevicePattern([uint32]$pairId, [string]$role) {
    $hex = '{0:X8}' -f $pairId
    # 实例 ID 中间的 "1&xxxx&N&" 段会随设备重建变化，不能写死。
    return "COM0COM\PORT_$role\*C0C0_$hex$role"
}

function Get-PresentPairDevices([uint32]$pairId) {
    $devices = @()
    foreach ($role in 'A', 'B') {
        $pattern = Get-PairDevicePattern $pairId $role
        $devices += @(Get-PnpDevice -ErrorAction SilentlyContinue |
            Where-Object { $_.Present -and $_.InstanceId -like $pattern })
    }
    return $devices
}

function Get-DeviceSummary([uint32]$pairId) {
    $rows = @()
    foreach ($role in 'A', 'B') {
        $pattern = Get-PairDevicePattern $pairId $role
        foreach ($dev in @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -like $pattern })) {
            $rows += "$($dev.InstanceId) Status=$($dev.Status) Problem=$($dev.Problem) Present=$($dev.Present)"
        }
    }
    return ($rows -join ' ; ')
}

function Wait-ForPairReady([uint32]$pairId, [int]$timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    do {
        $devices = @(Get-PresentPairDevices $pairId)
        $bad = @($devices | Where-Object { $_.Status -ne 'OK' })
        if ($devices.Count -ge 2 -and $bad.Count -eq 0) { return $true }
        Start-Sleep -Seconds 1
    } while ((Get-Date) -lt $deadline)
    return $false
}

function Get-DriverInfo([string]$name) {
    $svc = Get-CimInstance Win32_SystemDriver -Filter "Name='$name'" -ErrorAction SilentlyContinue
    if (-not $svc) {
        L "driver| $name service missing"
        return $null
    }
    $hash = if ($svc.PathName -and (Test-Path $svc.PathName)) {
        (Get-FileHash $svc.PathName -Algorithm SHA256).Hash
    } else { 'MISSING' }
    L "driver| $name path=$($svc.PathName) hash=$hash state=$($svc.State)"
    return [pscustomobject]@{ Name = $name; Path = $svc.PathName; Hash = $hash; State = $svc.State }
}

function Get-SerialCommValues {
    try {
        $props = Get-ItemProperty 'HKLM:\HARDWARE\DEVICEMAP\SERIALCOMM' -ErrorAction Stop
        return @($props.PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' } |
            ForEach-Object { [string]$_.Value })
    }
    catch {
        return @()
    }
}

function Wait-ForSerialCommRemoved([string[]]$names, [int]$timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    do {
        $values = @(Get-SerialCommValues)
        if (-not ($names | Where-Object { $values -contains $_ })) { return $true }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    return $false
}

function Test-Transfer([string]$tx, [string]$rx, [int]$count, [string]$direction) {
    $a = $null
    $b = $null
    try {
        $a = New-Object System.IO.Ports.SerialPort $tx, 115200, 'None', 8, 'One'
        $b = New-Object System.IO.Ports.SerialPort $rx, 115200, 'None', 8, 'One'
        $a.ReadTimeout = 3000; $b.ReadTimeout = 3000
        $a.WriteTimeout = 3000; $b.WriteTimeout = 3000
        $a.Open(); $b.Open()
        $null = $b.ReadExisting()
        $data = New-Object byte[] $count
        (New-Object System.Random 12345).NextBytes($data)
        $a.Write($data, 0, $count)
        $got = New-Object byte[] $count
        $offset = 0
        $deadline = (Get-Date).AddSeconds(8)
        while ($offset -lt $count -and (Get-Date) -lt $deadline) {
            try { $read = $b.Read($got, $offset, $count - $offset) } catch { break }
            if ($read -le 0) { break }
            $offset += $read
        }
        $match = ($offset -eq $count)
        if ($match) {
            for ($i = 0; $i -lt $count; $i++) {
                if ($data[$i] -ne $got[$i]) { $match = $false; break }
            }
        }
        L "xfer| $direction $tx->$rx bytes=$offset/$count match=$match"
        return $match
    }
    catch {
        L "xfer| $direction $tx->$rx ERROR $($_.Exception.Message)"
        return $false
    }
    finally {
        if ($a) { try { $a.Close() } catch {} }
        if ($b) { try { $b.Close() } catch {} }
    }
}

$exitCode = 1
try {
    L "host| $([System.Environment]::MachineName) user=$([System.Environment]::UserName)"
    if (-not (Test-Path $setupc)) { throw "找不到 setupc.exe: $setupc" }

    # ---- 阶段 0：清理已有 pair -------------------------------------------
    $existing = @(Get-PairIds)
    L "pairs| existing=$($existing -join ',')"
    foreach ($id in $existing) { $null = RunSetupc "cleanup-$id" @('remove', "$id") }
    if ($existing.Count -gt 0) { Start-Sleep -Seconds 3 }
    $leftover = @(Get-PairIds)
    Check 'cleanup| no pairs before test' ($leftover.Count -eq 0) "remaining=$($leftover -join ',')"

    # pair 删除后端口 devnode 会变成 phantom 并继续占用 COM 名；清掉它们，
    # 让本轮显式命名的端口拿到 COM15/16、COM20/21。
    foreach ($hardwareId in 'COM0COM\PORT_A', 'COM0COM\PORT_B') {
        $out = & pnputil.exe /remove-device /deviceid $hardwareId /force 2>&1 | Out-String
        L "phantom-cleanup| $hardwareId exit=$LASTEXITCODE"
        L $out.Trim()
    }
    Start-Sleep -Seconds 2
    $initialPorts = @(Get-Ports)
    $namesFree = -not ($initialPorts -contains $PairAPort) -and -not ($initialPorts -contains $PairBPort) -and
        -not ($initialPorts -contains $PairCPort) -and -not ($initialPorts -contains $PairDPort)
    Check 'cleanup| target COM names free' $namesFree "ports=$($initialPorts -join ',')"

    # ---- 阶段 1：显式命名 pair 0 ----------------------------------------
    L '--- pair 0: explicit COM15/COM16 ---'
    $result = RunSetupc 'add-0' @('add', '--id', '0', '--port-a', $PairAPort, '--port-b', $PairBPort)
    Check 'pair0| add' ($result.ExitCode -eq 0 -and $result.Text -match 'Created pair 0') (Brief $result.Text)

    $up = Wait-ForPairReady 0 $PairTimeoutSec
    Check 'pair0| devices OK' $up (Get-DeviceSummary 0)
    $portsUp = Wait-ForPorts @($PairAPort, $PairBPort) $true 30
    Check 'pair0| ports present' $portsUp "ports=$((Get-Ports) -join ',')"

    $bus = Get-DriverInfo 'com0combus'
    $busOk = [bool]($bus -and $bus.State -eq 'Running')
    Check 'driver| com0combus running' $busOk $(if ($bus) { "state=$($bus.State)" } else { 'service missing' })
    $busHashOk = [bool]($bus -and ($ExpectedBusHash -eq '' -or $bus.Hash -eq $ExpectedBusHash))
    Check 'driver| com0combus hash matches build' $busHashOk "expected=$ExpectedBusHash actual=$(if ($bus) { $bus.Hash } else { 'n/a' })"

    $port = Get-DriverInfo 'com0comport'
    $portOk = [bool]($port -and $port.State -eq 'Running')
    Check 'driver| com0comport running' $portOk $(if ($port) { "state=$($port.State)" } else { 'service missing' })
    $portHashOk = [bool]($port -and ($ExpectedPortHash -eq '' -or $port.Hash -eq $ExpectedPortHash))
    Check 'driver| com0comport hash matches build' $portHashOk "expected=$ExpectedPortHash actual=$(if ($port) { $port.Hash } else { 'n/a' })"

    Check 'pair0| transfer A->B 4096 bytes' (Test-Transfer $PairAPort $PairBPort 4096 'A->B')
    Check 'pair0| transfer B->A 4096 bytes' (Test-Transfer $PairBPort $PairAPort 4096 'B->A')

    # ---- 阶段 2：删除 pair 0 --------------------------------------------
    L '--- remove pair 0 ---'
    $result = RunSetupc 'remove-0' @('remove', '0')
    Check 'pair0| remove' ($result.ExitCode -eq 0) (Brief $result.Text)
    $portsGone = Wait-ForPorts @($PairAPort, $PairBPort) $false 30
    Check 'pair0| ports removed' $portsGone "ports=$((Get-Ports) -join ',')"
    Start-Sleep -Seconds 2
    $presentLeft = @(Get-PresentPairDevices 0)
    Check 'pair0| no present devnodes' ($presentLeft.Count -eq 0) (Get-DeviceSummary 0)
    $scGone = Wait-ForSerialCommRemoved @($PairAPort, $PairBPort) 15
    Check 'pair0| SERIALCOMM cleared' $scGone "values=$((Get-SerialCommValues) -join ',')"
    $list = RunSetupc 'list-after-0' @('list')
    Check 'pair0| list empty' ($list.Text -match '\(no pairs\)') (Brief $list.Text)

    # ---- 阶段 3：第二对（id 5）------------------------------------------
    L '--- pair 5: explicit COM20/COM21 ---'
    $result = RunSetupc 'add-5' @('add', '--id', '5', '--port-a', $PairCPort, '--port-b', $PairDPort)
    Check 'pair5| add' ($result.ExitCode -eq 0 -and $result.Text -match 'Created pair 5') (Brief $result.Text)
    Check 'pair5| devices OK' (Wait-ForPairReady 5 $PairTimeoutSec) (Get-DeviceSummary 5)
    Check 'pair5| ports present' (Wait-ForPorts @($PairCPort, $PairDPort) $true 30) "ports=$((Get-Ports) -join ',')"
    Check 'pair5| transfer A->B 1024 bytes' (Test-Transfer $PairCPort $PairDPort 1024 'A->B')
    Check 'pair5| transfer B->A 1024 bytes' (Test-Transfer $PairDPort $PairCPort 1024 'B->A')

    # 仿真写路径回归：v8 在 emu_br 打开时以 NULL 时间戳触发 0xA，必须实机覆盖。
    $result = RunSetupc 'set-5-emu' @('set', '5', '--baud-emu', '--overrun')
    Check 'pair5| enable baud emulation' ($result.ExitCode -eq 0 -and $result.Text -match 'Updated pair 5') (Brief $result.Text)
    $result = RunSetupc 'params-5-emu' @('params', '5')
    $flagsOk = $result.Text -match 'baud-emu, overrun'
    Check 'pair5| params report baud-emu+overrun' $flagsOk (Brief $result.Text)
    Check 'pair5| transfer with baud emulation' (Test-Transfer $PairCPort $PairDPort 256 'emu A->B')

    $result = RunSetupc 'remove-5' @('remove', '5')
    Check 'pair5| remove' ($result.ExitCode -eq 0) (Brief $result.Text)
    Check 'pair5| ports removed' (Wait-ForPorts @($PairCPort, $PairDPort) $false 30) "ports=$((Get-Ports) -join ',')"
    $list = RunSetupc 'list-after-5' @('list')
    Check 'pair5| list empty' ($list.Text -match '\(no pairs\)') (Brief $list.Text)

    # ---- 阶段 4：自动命名 pair 7 ----------------------------------------
    L '--- pair 7: auto-assigned names ---'
    $beforePorts = @(Get-Ports)
    $result = RunSetupc 'add-7' @('add', '--id', '7')
    Check 'auto| add' ($result.ExitCode -eq 0 -and $result.Text -match 'Created pair 7') (Brief $result.Text)
    Check 'auto| devices OK' (Wait-ForPairReady 7 $PairTimeoutSec) (Get-DeviceSummary 7)

    $autoNames = @()
    $deadline = (Get-Date).AddSeconds(30)
    do {
        $current = @(Get-Ports)
        $autoNames = @($current | Where-Object { $beforePorts -notcontains $_ } | Select-Object -First 2)
        if ($autoNames.Count -ge 2) { break }
        Start-Sleep -Seconds 1
    } while ((Get-Date) -lt $deadline)
    L "ports| before=$($beforePorts -join ',') after=$((Get-Ports) -join ',')"
    $distinct = ($autoNames.Count -eq 2 -and $autoNames[0] -ne $autoNames[1])
    Check 'auto| two distinct COM names' $distinct "names=$($autoNames -join ',')"
    Check 'auto| ports present' (Wait-ForPorts $autoNames $true 15) "ports=$((Get-Ports) -join ',')"

    $result = RunSetupc 'remove-7' @('remove', '7')
    Check 'auto| remove' ($result.ExitCode -eq 0) (Brief $result.Text)
    Check 'auto| ports removed' (Wait-ForPorts $autoNames $false 30) "ports=$((Get-Ports) -join ',')"

    # ---- 阶段 5：SDK / C ABI / daemon 冒烟 ------------------------------
    L '--- sdk / capi / daemon smoke ---'
    $sdkSmoke = Join-Path $BaseDir 'sdk-smoke.exe'
    $capiSmoke = Join-Path $BaseDir 'capi-smoke.exe'
    $daemonExe = Join-Path $BaseDir 'com0comd.exe'
    $capiDll = Join-Path $BaseDir 'com0com_capi.dll'
    Check 'capi| com0com_capi.dll packaged' (Test-Path $capiDll)

    $result = Run-Tool 'capi-self-test' $capiSmoke @('--self-test')
    Check 'capi| self-test' ($result.ExitCode -eq 0) (Brief $result.Text)

    $result = Run-Tool 'capi-native' $capiSmoke @('--native')
    Check 'capi| native smoke' ($result.ExitCode -eq 0) (Brief $result.Text)

    $result = Run-Tool 'sdk-fake' $sdkSmoke @('--fake')
    Check 'sdk| fake smoke' ($result.ExitCode -eq 0) (Brief $result.Text)

    $result = Run-Tool 'sdk-native' $sdkSmoke @('--native')
    Check 'sdk| native smoke' ($result.ExitCode -eq 0) (Brief $result.Text)

    $result = Run-Tool 'rpc-selftest' $daemonExe @('selftest')
    Check 'rpc| daemon selftest' ($result.ExitCode -eq 0) (Brief $result.Text)

    # ---- 阶段 6：daemon JSON-RPC + --autosave/--restore -----------------
    L '--- daemon rpc: autosave / restore ---'
    $pipe = 'com0com-vmtest'
    $statePath = Join-Path $BaseDir 'state.json'
    $daemonLog = Join-Path $BaseDir 'com0comd.log'
    Remove-Item $statePath -Force -ErrorAction SilentlyContinue
    Remove-Item $daemonLog -Force -ErrorAction SilentlyContinue
    Remove-Item "$daemonLog.stdout" -Force -ErrorAction SilentlyContinue
    Remove-Item "$daemonLog.stderr" -Force -ErrorAction SilentlyContinue
    $null = Run-Tool 'rpc-preflight' $daemonExe @('stop', '--pipe', $pipe) 15000

    $daemon = $null
    try {
        $daemon = Start-Daemon $daemonExe $pipe $daemonLog $statePath $false
        $status = Wait-DaemonStatus $daemonExe $pipe 30
        Check 'rpc| daemon started' ($status.ExitCode -eq 0) (Brief $status.Text)

        $result = Run-Tool 'rpc-sdk' $sdkSmoke @('--daemon', $pipe)
        Check 'rpc| sdk smoke over daemon' ($result.ExitCode -eq 0) (Brief $result.Text)

        # 在 daemon 运行期间用 setupc 建 pair 9，退出时的最终 autosave 会把
        # 驱动里的真实状态写进 state.json。
        $result = RunSetupc 'restore-seed' @('add', '--id', '9', '--port-a', 'COM25', '--port-b', 'COM26')
        if ($result.ExitCode -ne 0) { L "restore| seed failed: $(Brief $result.Text)" }
        $null = Wait-ForPairReady 9 $PairTimeoutSec

        $result = Run-Tool 'rpc-stop' $daemonExe @('stop', '--pipe', $pipe) 30000
        $exited = $daemon.WaitForExit(15000)
        Check 'rpc| daemon stop' ($result.ExitCode -eq 0 -and $exited) (Brief $result.Text)
    }
    finally {
        Stop-DaemonProcess $daemon
    }
    Check 'rpc| autosave wrote state file' (Test-Path $statePath) "path=$statePath"

    $null = RunSetupc 'restore-clear' @('remove', '9')
    $null = Wait-ForPorts @('COM25', 'COM26') $false 30
    Start-Sleep -Seconds 2

    $daemon = $null
    try {
        $daemon = Start-Daemon $daemonExe $pipe $daemonLog $statePath $true
        $status = Wait-DaemonStatus $daemonExe $pipe 30
        Check 'rpc| daemon restarted with --restore' ($status.ExitCode -eq 0) (Brief $status.Text)

        $list = RunSetupc 'restore-list' @('list')
        Check 'restore| pair 9 recreated' ($list.Text -match '(?m)^\s*9\s+') (Brief $list.Text)
        Check 'restore| ports restored' (Wait-ForPorts @('COM25', 'COM26') $true 30) "ports=$((Get-Ports) -join ',')"

        $null = Run-Tool 'rpc-stop-restore' $daemonExe @('stop', '--pipe', $pipe) 30000
        $null = $daemon.WaitForExit(15000)
    }
    finally {
        Stop-DaemonProcess $daemon
    }

    $null = RunSetupc 'restore-cleanup' @('remove', '9')
    Start-Sleep -Seconds 2

    $result = RunSetupc 'list-final' @('list')
    Check 'cleanup| final list empty' ($result.Text -match '\(no pairs\)') (Brief $result.Text)

    if ($script:failures -eq 0) { $exitCode = 0 }
}
catch {
    L "ERROR| $($_.Exception.Message)"
    $exitCode = 1
}

$status = if ($exitCode -eq 0) { 'PASS' } else { 'FAIL' }
L "RESULT| $status checks=$($script:checks) failures=$($script:failures)"
"=== done (exit $exitCode) ===" | Out-File $log -Append -Encoding utf8
exit $exitCode
