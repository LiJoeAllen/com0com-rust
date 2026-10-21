# ---------------------------------------------------------------------------
# 第三方依赖版本一致性检查。
#
# 唯一版本源：根 Cargo.toml 的 [workspace.dependencies]。
# 两个内核驱动各自是独立 workspace 根（wdk-build 不允许 KMDF/WDM 混在同一
# 依赖图），无法继承根表，只能重复声明；本脚本保证它们的版本与根完全一致，
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

# 把一行依赖值转成条目。支持三种形式：
#   "0.61"                       → Version
#   { path = "..." }             → Path（第一方本地依赖）
#   { path|version = ..., ... }  → 对应字段
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
        Workspace = $false
        Line      = $LineNo
    }

    if ($Value -match '^"([^"]+)"$') {
        $entry.Version = $Matches[1]
        return $entry
    }

    if ($Value -match '^\{.*\}$') {
        if ($Value -match 'path\s*=\s*"([^"]+)"') { $entry.Path = $Matches[1] }
        if ($Value -match 'version\s*=\s*"([^"]+)"') { $entry.Version = $Matches[1] }
        if ($Value -match 'workspace\s*=\s*true') { $entry.Workspace = $true }
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

# 根 [workspace.dependencies] 中的第三方版本即为规范版本。
$canonical = @{}
foreach ($e in $byManifest['root']) {
    if ($e.Section -eq 'workspace.dependencies' -and $e.Version -and -not $e.Path) {
        $canonical[$e.Name] = $e.Version
    }
}
if ($canonical.Count -eq 0) {
    throw '根 Cargo.toml 的 [workspace.dependencies] 中没有找到第三方依赖。'
}

$errors = New-Object System.Collections.ArrayList
foreach ($m in $manifests) {
    foreach ($e in $byManifest[$m.Name]) {
        if ($e.Section -eq 'workspace.dependencies') {
            if (-not $e.Version) { continue }   # 本地 path 依赖不参与版本比对
            if (-not $canonical.ContainsKey($e.Name)) {
                [void]$errors.Add("$($m.Name): $($e.Name) 未在根 Cargo.toml 的 [workspace.dependencies] 中登记")
            }
            elseif ($canonical[$e.Name] -ne $e.Version) {
                [void]$errors.Add(
                    "$($m.Name): $($e.Name) 版本 `"$($e.Version)`" 与根版本 `"$($canonical[$e.Name])`" 不一致"
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
    Write-Host '请先在根 Cargo.toml 的 [workspace.dependencies] 中更新版本，再同步各独立 workspace 根。' -ForegroundColor Yellow
    exit 1
}

$names = ($canonical.Keys | Sort-Object) -join ', '
Write-Host "[check-deps] OK：$($canonical.Count) 个第三方依赖版本一致（$names）。"
exit 0
