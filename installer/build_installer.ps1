#Requires -Version 5.1
<#
.SYNOPSIS
    一键打包「应用看门狗」Windows 安装程序（NSIS）。

.DESCRIPTION
    步骤：
      1. 刷新版本号——touch watchdog-core/src/lib.rs 触发重编（版本号 = 本次编译时间，
         见 crates\watchdog-core\build.rs）；
      2. cargo build --release；
      3. 从 watchdog.exe version 读取版本号；
      4. 确保 NSIS 便携版就位（installer\nsis\makensis.exe，缺失时自动从 SourceForge 下载）；
      5. makensis 编译安装包 → installer\dist\watchdog-setup-<版本>.exe

    注意：若看门狗服务正从 target\release\watchdog.exe 运行，文件被占用会导致构建失败，
    请先以管理员执行 sc stop watchdog，打包完成后再启动。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File installer\build_installer.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File installer\build_installer.ps1 -NoVersionBump
    # 不刷新版本号（保留上次编译时间戳），仅重新打包
#>
param(
    [string]$Cargo = "C:\Users\admin\.cargo\bin\cargo.exe",
    [switch]$NoVersionBump
)

$ErrorActionPreference = "Stop"
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }   # 控制台中文输出不乱码
$installerDir = $PSScriptRoot
$root = Split-Path -Parent $installerDir
$nsisDir = Join-Path $installerDir "nsis"
$distDir = Join-Path $installerDir "dist"
$nsisVersion = "3.11"   # 便携 zip 版本号，缺失时自动下载

# ---- 1. 刷新版本号：touch build.rs 触发构建脚本重跑 ----
# 注意：build.rs 声明了 rerun-if-changed=build.rs，版本号（编译时间）由 build.rs 生成——
# 必须更新 build.rs 的修改时间让其重跑，仅 touch 源码文件只会重编译、版本号不变。
$buildRs = Join-Path $root "crates\watchdog-core\build.rs"
if (-not $NoVersionBump) {
    (Get-Item -LiteralPath $buildRs).LastWriteTime = Get-Date
    Write-Host "==> 已刷新版本号触发重编（版本号 = 本次编译时间）"
}

# ---- 2. cargo build --release ----
Write-Host "==> cargo build --release ..."
& $Cargo build --release --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Warning "cargo build 失败（exit $LASTEXITCODE）。"
    Write-Warning "若提示「拒绝访问 (os error 5)」：看门狗服务可能正从 target\release\watchdog.exe 运行，"
    Write-Warning "请先以管理员执行 sc stop watchdog 再重试，打包完成后再 sc start watchdog。"
    throw "cargo build --release 失败（exit $LASTEXITCODE）"
}

$watchdogExe = Join-Path $root "target\release\watchdog.exe"
if (-not (Test-Path -LiteralPath $watchdogExe)) { throw "未找到构建产物：$watchdogExe" }

# ---- 3. 读取版本号（watchdog.exe version → 形如 2026.1123.1931）----
$verOutput = (& $watchdogExe version | Out-String)
if ($verOutput -notmatch "(\d{4})\.(\d{4})\.(\d{4})") {
    throw "无法从 version 输出解析版本号：$verOutput"
}
$verRaw = $Matches[1] + "." + $Matches[2] + "." + $Matches[3]           # 2026.1123.1931
$verNsi = "{0}.{1}.{2}.{3}" -f $Matches[1], `
    $Matches[2].Substring(0, 2), $Matches[2].Substring(2, 2), $Matches[3]  # 2026.11.23.1931
Write-Host "==> 版本号：$verRaw"

# ---- 4. NSIS 便携版（缺失时自动下载，解压到 installer\nsis，不改动系统）----
$makensis = Join-Path $nsisDir "makensis.exe"
if (-not (Test-Path -LiteralPath $makensis)) {
    Write-Host "==> 未找到 NSIS，下载便携版 $nsisVersion ..."
    $downloadDir = Join-Path $installerDir "_download"
    $extractDir = Join-Path $downloadDir "extract"
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
    $zipPath = Join-Path $downloadDir ("nsis-$nsisVersion.zip")
    $urls = @(
        "https://downloads.sourceforge.net/project/nsis/NSIS%203/$nsisVersion/nsis-$nsisVersion.zip",
        "https://prdownloads.sourceforge.net/nsis/nsis-$nsisVersion.zip"
    )
    $downloaded = $false
    foreach ($u in $urls) {
        Write-Host "    下载：$u"
        & curl.exe -L --fail --retry 2 --connect-timeout 20 -o $zipPath $u
        if ($LASTEXITCODE -eq 0) { $downloaded = $true; break }
    }
    if (-not $downloaded) {
        throw "NSIS 下载失败，请手动下载 nsis-$nsisVersion.zip 解压到 $nsisDir 后重试"
    }
    Write-Host "==> 解压 NSIS ..."
    Expand-Archive -LiteralPath $zipPath -DestinationPath $extractDir -Force
    $inner = Get-ChildItem -LiteralPath $extractDir -Directory | Select-Object -First 1
    if (-not $inner) { throw "解压结果异常：$extractDir" }
    if (Test-Path -LiteralPath $nsisDir) { Remove-Item -Recurse -Force $nsisDir }
    Move-Item -LiteralPath $inner.FullName -Destination $nsisDir
    Remove-Item -Recurse -Force $downloadDir
}

# .nsi 必须为 UTF-8 with BOM（makensis 以 BOM 识别 UTF-8，否则按 ANSI 解析中文会乱码）
$nsiPath = Join-Path $installerDir "watchdog.nsi"
$nsiText = [System.IO.File]::ReadAllText($nsiPath, [System.Text.Encoding]::UTF8)
[System.IO.File]::WriteAllText($nsiPath, $nsiText, (New-Object System.Text.UTF8Encoding($true)))

# ---- 5. makensis 编译安装包（相对路径以 installer 目录为基准）----
if (-not (Test-Path -LiteralPath $distDir)) {
    New-Item -ItemType Directory -Force -Path $distDir | Out-Null
}
Write-Host "==> 编译安装包（makensis）..."
Push-Location $installerDir
try {
    & $makensis "/DSETUP_VERSION_RAW=$verRaw" "/DSETUP_VERSION=$verNsi" "watchdog.nsi"
    if ($LASTEXITCODE -ne 0) { throw "makensis 失败（exit $LASTEXITCODE）" }
} finally {
    Pop-Location
}

$outFile = Join-Path $distDir "watchdog-setup-$verRaw.exe"
if (-not (Test-Path -LiteralPath $outFile)) { throw "打包完成但未找到产物：$outFile" }
Write-Host ""
Write-Host ("==> 完成：{0}（{1:N1} MB）" -f $outFile, ((Get-Item -LiteralPath $outFile).Length / 1MB))
