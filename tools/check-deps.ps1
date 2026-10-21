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
# 用法：powershell -NoProfile -ExecutionPolicy Bypass -File tools/check-deps.ps1
# 退出码：0 = 一致；1 = 存在漂移或未集中管理的依赖。
# ---------------------------------------------------------------------------
param(
    [string]$RepoRoot = ''
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

$manifests | ForEach-Object {
    if (-not (Test-Path -LiteralPath $_.Path)) {
        throw "找不到清单：$($_.Path)"
    }
}

$byManifest = @{}
foreach ($m in $manifests) {
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

$errors = New-Object System.Collections.ArrayList

$canonicalWdkBuild = $canonical['wdk-build'] -replace '^version=', ''
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

foreach ($m in $manifests) {
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
