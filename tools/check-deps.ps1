# ---------------------------------------------------------------------------
# 第三方依赖版本一致性检查。
#
# 唯一版本源：根 Cargo.toml 的 [workspace.dependencies]。
# 两个内核驱动各自是独立 workspace 根（wdk-build 不允许 KMDF/WDM 混在同一
# 依赖图），无法继承根表，只能重复声明；两个 Makefile.toml 的 cargo-make
# 加载脚本还内嵌了一份 wdk-build 依赖。本脚本保证这些副本与根完全一致，
# 并禁止包内的 [dependencies]/[build-dependencies]/[dev-dependencies] 直接
# 写版本号（必须用 workspace = true 或本地 path）。
#
# 用法：
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/check-deps.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/check-deps.ps1 -Fix
#
# -Fix 把两个驱动清单的 [workspace.package] 同名字段、[workspace.dependencies]
# 中已登记的第三方条目，以及 Makefile.toml 内嵌的 wdk-build 版本，重写为根
# Cargo.toml 中的值；本地 path 依赖与驱动未使用的依赖不会被增删。
# 退出码：0 = 一致（或已同步为一致）；1 = 存在漂移或未集中管理的依赖。
# ---------------------------------------------------------------------------
param(
    [string]$RepoRoot = '',
    [switch]$Fix
)

$ErrorActionPreference = 'Stop'

if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

$manifests = @(
    @{ Name = 'root';        Path = (Join-Path $RepoRoot 'Cargo.toml') },
    @{ Name = 'com0combus';  Path = (Join-Path $RepoRoot 'crates\com0combus\Cargo.toml') },
    @{ Name = 'com0comport'; Path = (Join-Path $RepoRoot 'crates\com0comport\Cargo.toml') }
)

# 两个驱动的 cargo-make 加载脚本（Makefile.toml）内嵌了独立的
# wdk-build 依赖，无法继承 workspace，也必须与根版本保持一致。
$makefiles = @(
    @{ Name = 'com0combus/Makefile.toml';  Path = (Join-Path $RepoRoot 'crates\com0combus\Makefile.toml') },
    @{ Name = 'com0comport/Makefile.toml'; Path = (Join-Path $RepoRoot 'crates\com0comport\Makefile.toml') }
)

# 根 workspace 的普通成员（SDK/backend/工具）：它们继承根 workspace，不需要
# [workspace.package]，但包内依赖必须使用 workspace = true 或本地 path。
$memberManifests = @(
    @{ Name = 'com0com-core';        Path = (Join-Path $RepoRoot 'crates\com0com-core\Cargo.toml') },
    @{ Name = 'com0com-backend-win'; Path = (Join-Path $RepoRoot 'crates\com0com-backend-win\Cargo.toml') },
    @{ Name = 'com0com-backend-pty'; Path = (Join-Path $RepoRoot 'crates\com0com-backend-pty\Cargo.toml') },
    @{ Name = 'com0com-backend-stub'; Path = (Join-Path $RepoRoot 'crates\com0com-backend-stub\Cargo.toml') },
    @{ Name = 'com0com-sdk';         Path = (Join-Path $RepoRoot 'crates\com0com-sdk\Cargo.toml') },
    @{ Name = 'com0com-ipc';         Path = (Join-Path $RepoRoot 'crates\com0com-ipc\Cargo.toml') },
    @{ Name = 'com0com-daemon';      Path = (Join-Path $RepoRoot 'crates\com0com-daemon\Cargo.toml') },
    @{ Name = 'com0com-capi';        Path = (Join-Path $RepoRoot 'crates\com0com-capi\Cargo.toml') },
    @{ Name = 'setupc';              Path = (Join-Path $RepoRoot 'tools\setupc\Cargo.toml') },
    @{ Name = 'sdk-smoke';           Path = (Join-Path $RepoRoot 'tools\sdk-smoke\Cargo.toml') },
    @{ Name = 'capi-smoke';          Path = (Join-Path $RepoRoot 'tools\capi-smoke\Cargo.toml') },
    @{ Name = 'header-gen';          Path = (Join-Path $RepoRoot 'tools\header-gen\Cargo.toml') }
)

$allManifests = @($manifests) + @($memberManifests)

# 把一行依赖值转成条目。支持三种形式：
#   "0.61"                       → Version
#   { path = "..." }             → Path（第一方本地依赖）
#   { path|version = ..., ... }  → 对应字段
#   { git = "...", tag = "..." } → Git/Tag/Rev/Branch（GitHub 等非 crates.io 源）
function ConvertTo-DependencyEntry {
    param(
        [string]$Manifest,
        [int]$LineNo,
        [string]$Name,
        [string]$Section,
        [string]$Value
    )

    $entry = @{
        Name      = $Name
        Section   = $Section
        Version   = $null
        Path      = $null
        Git       = $null
        Tag       = $null
        Rev       = $null
        Branch    = $null
        Source    = $null
        Workspace = $false
        Line      = $LineNo
    }

    if ($Value -match '^"([^"]+)"$') {
        $entry.Version = $Matches[1]
        $entry.Source = "version=$($entry.Version)"
        return $entry
    }

    if ($Value -match '^\{.*\}$') {
        if ($Value -match 'path\s*=\s*"([^"]+)"') { $entry.Path = $Matches[1] }
        if ($Value -match 'version\s*=\s*"([^"]+)"') { $entry.Version = $Matches[1] }
        if ($Value -match 'workspace\s*=\s*true') { $entry.Workspace = $true }
        if ($Value -match '\bgit\s*=\s*"([^"]+)"') { $entry.Git = $Matches[1] }
        if ($Value -match '\btag\s*=\s*"([^"]+)"') { $entry.Tag = $Matches[1] }
        if ($Value -match '\brev\s*=\s*"([^"]+)"') { $entry.Rev = $Matches[1] }
        if ($Value -match '\bbranch\s*=\s*"([^"]+)"') { $entry.Branch = $Matches[1] }

        # 版本与 Git 源都可作为“来源标识”，供三个 workspace 根比对。
        if ($entry.Version) {
            $entry.Source = "version=$($entry.Version)"
        }
        elseif ($entry.Git) {
            $refs = @()
            if ($entry.Tag) { $refs += "tag=$($entry.Tag)" }
            if ($entry.Rev) { $refs += "rev=$($entry.Rev)" }
            if ($entry.Branch) { $refs += "branch=$($entry.Branch)" }
            $entry.Source = "git=$($entry.Git)"
            if ($refs.Count -gt 0) { $entry.Source += ' ' + ($refs -join ' ') }
        }
        return $entry
    }

    throw "${Manifest}:$LineNo 无法解析的依赖值：$Value"
}

# 读取清单中所有依赖段的条目；跨行的内联表（如 windows-sys 的 features）
# 会先拼接再解析。
function Read-DependencySections {
    param([string]$Manifest)

    $entries = New-Object System.Collections.ArrayList
    $section = ''
    $pending = $null
    $lineNo = 0

    foreach ($raw in Get-Content -LiteralPath $Manifest -Encoding UTF8) {
        $lineNo++
        $line = $raw.Trim()

        if ($null -ne $pending) {
            $pending.Text += ' ' + $line
            if ($line -match '}') {
                $entry = ConvertTo-DependencyEntry -Manifest $Manifest -LineNo $pending.Line `
                    -Name $pending.Name -Section $pending.Section -Value $pending.Text
                [void]$entries.Add($entry)
                $pending = $null
            }
            continue
        }

        if ($line -eq '' -or $line.StartsWith('#')) { continue }
        if ($line -match '^\[(.+?)\]$') { $section = $Matches[1]; continue }

        $isWorkspaceDeps = $section -eq 'workspace.dependencies'
        $isPackageDeps = $section -match '(^|\.)(dependencies|build-dependencies|dev-dependencies)$'
        if (-not ($isWorkspaceDeps -or $isPackageDeps)) { continue }

        if ($line -notmatch '^([A-Za-z0-9_-]+)\s*=\s*(.+)$') {
            throw "${Manifest}:$lineNo 无法解析的依赖声明：$line"
        }
        $name = $Matches[1]
        $value = $Matches[2].Trim()

        if ($value.StartsWith('{') -and $value -notmatch '}') {
            $pending = @{ Name = $name; Section = $section; Text = $value; Line = $lineNo }
            continue
        }

        $entry = ConvertTo-DependencyEntry -Manifest $Manifest -LineNo $lineNo `
            -Name $name -Section $section -Value $value
        [void]$entries.Add($entry)
    }

    if ($null -ne $pending) {
        throw "${Manifest}:$($pending.Line) 内联依赖表缺少右花括号"
    }

    return ,$entries
}

# 读取 [workspace.package] 的字符串字段（version / edition / license 等）。
function Read-WorkspacePackageValues {
    param([string]$Manifest)

    $values = @{}
    $inSection = $false
    foreach ($line in [System.IO.File]::ReadAllLines($Manifest)) {
        $trim = $line.Trim()
        if ($trim -match '^\[(.+?)\]$') {
            $inSection = ($Matches[1] -eq 'workspace.package')
            continue
        }
        if (-not $inSection) { continue }
        if ($trim -match '^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"') {
            $values[$Matches[1]] = $Matches[2]
        }
    }
    return $values
}

# 读取 [workspace.dependencies] 中每个声明的行区间（0 基，End 含）。
function Read-WorkspaceDependencyBlocks {
    param([string]$Manifest)

    $lines = [System.IO.File]::ReadAllLines($Manifest)
    $blocks = @{}
    $inSection = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i].Trim()
        if ($line -match '^\[(.+?)\]$') {
            $inSection = ($Matches[1] -eq 'workspace.dependencies')
            continue
        }
        if (-not $inSection -or $line -eq '' -or $line.StartsWith('#')) { continue }
        if ($line -notmatch '^([A-Za-z0-9_-]+)\s*=\s*(.+)$') { continue }

        # 先保存匹配结果：下面的循环还会执行 -notmatch，会覆盖 $Matches。
        $name = $Matches[1]
        $value = $Matches[2].Trim()
        $end = $i
        if ($value.StartsWith('{') -and $value -notmatch '}') {
            while ($end + 1 -lt $lines.Count -and $lines[$end] -notmatch '}') { $end++ }
            if ($lines[$end] -notmatch '}') {
                throw "${Manifest}:$($i + 1) 内联依赖表缺少右花括号"
            }
        }
        $blocks[$name] = @{ Start = $i; End = $end }
        $i = $end
    }
    return $blocks
}

# 按原文件的行尾与 BOM 风格写回，避免 -Fix 产生无关 diff。
function Write-ManifestText {
    param([string]$Manifest, [string]$Text)

    $bytes = [System.IO.File]::ReadAllBytes($Manifest)
    $hasBom = ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)
    [System.IO.File]::WriteAllText($Manifest, $Text, (New-Object System.Text.UTF8Encoding -ArgumentList $hasBom))
}

# 把单个清单同步为根值；返回是否有改动。
function Sync-Manifest {
    param(
        [string]$Manifest,
        [hashtable]$RootPackage,
        [string[]]$RootDependencyLines,
        [hashtable]$RootDependencyBlocks,
        [hashtable]$Canonical,
        [System.Collections.ArrayList]$Changes
    )

    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.AddRange([string[]][System.IO.File]::ReadAllLines($Manifest))
    $modified = $false

    # [workspace.package]：只同步两边都存在的键，不新增字段。
    $inSection = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        $trim = $line.Trim()
        if ($trim -match '^\[(.+?)\]$') {
            $inSection = ($Matches[1] -eq 'workspace.package')
            continue
        }
        if (-not $inSection) { continue }
        if ($trim -match '^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"') {
            $key = $Matches[1]
            $old = $Matches[2]
            if ($RootPackage.ContainsKey($key) -and $RootPackage[$key] -ne $old) {
                $new = $RootPackage[$key]
                $q1 = $line.IndexOf('"')
                $q2 = $line.LastIndexOf('"')
                $lines[$i] = $line.Substring(0, $q1 + 1) + $new + $line.Substring($q2)
                [void]$Changes.Add("${Manifest}: [workspace.package] $key = `"$old`" -> `"$new`"")
                $modified = $true
            }
        }
    }

    # [workspace.dependencies]：整条声明替换为根声明。从后往前处理，
    # 避免前面的行号因增删行而漂移。
    $blocks = Read-WorkspaceDependencyBlocks -Manifest $Manifest
    $ordered = $blocks.Keys | Sort-Object { $blocks[$_].Start } -Descending
    foreach ($name in $ordered) {
        if (-not $Canonical.ContainsKey($name) -or -not $RootDependencyBlocks.ContainsKey($name)) { continue }
        $start = $blocks[$name].Start
        $count = $blocks[$name].End - $start + 1
        $rootBlock = $RootDependencyBlocks[$name]
        $replacement = @($RootDependencyLines[$rootBlock.Start..$rootBlock.End])
        if (($lines.GetRange($start, $count) -join "`n") -eq ($replacement -join "`n")) { continue }
        $lines.RemoveRange($start, $count)
        $lines.InsertRange($start, [string[]]$replacement)
        [void]$Changes.Add("${Manifest}: [workspace.dependencies] $name 同步为根声明")
        $modified = $true
    }

    if (-not $modified) { return }

    $bytes = [System.IO.File]::ReadAllBytes($Manifest)
    $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    $eol = if ($text.Contains("`r`n")) { "`r`n" } else { "`n" }
    $output = $lines -join $eol
    if ($text.EndsWith("`n")) { $output += $eol }
    Write-ManifestText -Manifest $Manifest -Text $output
}

# cargo-make 的 load_script 内嵌了独立的 wdk-build 依赖，也同步为根版本。
function Sync-MakefileVersion {
    param([string]$Manifest, [string]$Version, [System.Collections.ArrayList]$Changes)

    $bytes = [System.IO.File]::ReadAllBytes($Manifest)
    $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    $updated = [regex]::Replace($text, '(wdk-build\s*=\s*")[^"]+(")', {
        param($match)
        $match.Groups[1].Value + $Version + $match.Groups[2].Value
    })
    if ($updated -eq $text) { return }

    Write-ManifestText -Manifest $Manifest -Text $updated
    [void]$Changes.Add("${Manifest}: 内嵌 wdk-build 版本同步为 `"$Version`"")
}

$allManifests | ForEach-Object {
    if (-not (Test-Path -LiteralPath $_.Path)) {
        throw "找不到清单：$($_.Path)"
    }
}

$byManifest = @{}
foreach ($m in $allManifests) {
    $byManifest[$m.Name] = Read-DependencySections -Manifest $m.Path
}

# 根 [workspace.dependencies] 中的第三方来源（版本号或 Git 引用）即为规范来源。
$canonical = @{}
foreach ($e in $byManifest['root']) {
    if ($e.Section -eq 'workspace.dependencies' -and $e.Source -and -not $e.Path) {
        $canonical[$e.Name] = $e.Source
    }
}
if ($canonical.Count -eq 0) {
    throw '根 Cargo.toml 的 [workspace.dependencies] 中没有找到第三方依赖。'
}

$canonicalWdkBuild = $canonical['wdk-build'] -replace '^version=', ''
$rootPath = $manifests[0].Path
$rootPackageValues = Read-WorkspacePackageValues -Manifest $rootPath

if ($Fix) {
    $rootLines = [System.IO.File]::ReadAllLines($rootPath)
    $rootBlocks = Read-WorkspaceDependencyBlocks -Manifest $rootPath
    $changes = New-Object System.Collections.ArrayList

    foreach ($m in $manifests) {
        if ($m.Name -eq 'root') { continue }
        Sync-Manifest -Manifest $m.Path -RootPackage $rootPackageValues `
            -RootDependencyLines $rootLines -RootDependencyBlocks $rootBlocks `
            -Canonical $canonical -Changes $changes
    }

    if ($canonical['wdk-build'] -eq "version=$canonicalWdkBuild") {
        foreach ($mf in $makefiles) {
            if (Test-Path -LiteralPath $mf.Path) {
                Sync-MakefileVersion -Manifest $mf.Path -Version $canonicalWdkBuild -Changes $changes
            }
        }
    }

    if ($changes.Count -gt 0) {
        Write-Host "[check-deps] 已同步 $($changes.Count) 处：" -ForegroundColor Yellow
        foreach ($change in $changes) { Write-Host "  - $change" }
        # 重新读取，按修复后的内容做一致性校验。
        $byManifest = @{}
        foreach ($m in $allManifests) {
            $byManifest[$m.Name] = Read-DependencySections -Manifest $m.Path
        }
    }
    else {
        Write-Host '[check-deps] 无需同步：驱动清单与 Makefile.toml 已与根一致。'
    }
}

$errors = New-Object System.Collections.ArrayList

# [workspace.package]：三个 workspace 根必须声明同一组元数据。
foreach ($m in $manifests) {
    if ($m.Name -eq 'root') { continue }
    $values = Read-WorkspacePackageValues -Manifest $m.Path
    foreach ($key in $rootPackageValues.Keys) {
        if (-not $values.ContainsKey($key)) {
            [void]$errors.Add("$($m.Name): [workspace.package] 缺少 $key（根为 `"$($rootPackageValues[$key])`"）")
        }
        elseif ($values[$key] -ne $rootPackageValues[$key]) {
            [void]$errors.Add(
                "$($m.Name): [workspace.package] $key = `"$($values[$key])`" 与根值 `"$($rootPackageValues[$key])`" 不一致"
            )
        }
    }
}

foreach ($mf in $makefiles) {
    if (-not (Test-Path -LiteralPath $mf.Path)) {
        [void]$errors.Add("$($mf.Name): 找不到文件")
        continue
    }
    $match = Select-String -LiteralPath $mf.Path -Pattern 'wdk-build\s*=\s*"([^"]+)"' |
        Select-Object -First 1
    if (-not $match) {
        [void]$errors.Add("$($mf.Name): 未找到 wdk-build 版本声明")
        continue
    }
    $version = $match.Matches[0].Groups[1].Value
    if ($version -ne $canonicalWdkBuild) {
        [void]$errors.Add(
            "$($mf.Name): wdk-build 版本 `"$version`" 与根版本 `"$canonicalWdkBuild`" 不一致"
        )
    }
}

foreach ($m in $allManifests) {
    foreach ($e in $byManifest[$m.Name]) {
        if ($e.Section -eq 'workspace.dependencies') {
            if (-not $e.Source) { continue }   # 本地 path 依赖不参与来源比对
            if (-not $canonical.ContainsKey($e.Name)) {
                [void]$errors.Add("$($m.Name): $($e.Name) 未在根 Cargo.toml 的 [workspace.dependencies] 中登记")
            }
            elseif ($canonical[$e.Name] -ne $e.Source) {
                [void]$errors.Add(
                    "$($m.Name): $($e.Name) 来源 `"$($e.Source)`" 与根来源 `"$($canonical[$e.Name])`" 不一致"
                )
            }
            continue
        }

        # 包内依赖：必须继承 workspace 版本，或使用本地 path。
        if (-not $e.Workspace -and -not $e.Path) {
            [void]$errors.Add(
                "$($m.Name): [$($e.Section)] $($e.Name) 直接声明了版本 `"$($e.Version)`"，必须改用 workspace = true"
            )
        }
    }
}

if ($errors.Count -gt 0) {
    Write-Host '[check-deps] 依赖版本检查失败：' -ForegroundColor Red
    foreach ($err in $errors) {
        Write-Host "  - $err" -ForegroundColor Red
    }
    Write-Host '请先在根 Cargo.toml 的 [workspace.dependencies] 中更新版本/来源，再同步各独立 workspace 根。' -ForegroundColor Yellow
    exit 1
}

$names = ($canonical.Keys | Sort-Object) -join ', '
Write-Host "[check-deps] OK：$($canonical.Count) 个第三方依赖来源一致（$names）。"
exit 0
