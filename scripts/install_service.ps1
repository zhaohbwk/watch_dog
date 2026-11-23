#Requires -RunAsAdministrator
<#
.SYNOPSIS
    安装应用看门狗 Windows 服务（开机自启、看门狗自身崩溃自动重启）

.DESCRIPTION
    用 sc.exe 创建服务，binPath 指向 watchdog.exe，并配置失败后自动重启策略。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install_service.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install_service.ps1 -ExePath D:\tools\watchdog.exe -ConfigPath D:\tools\watchdog.toml
#>
param(
    [string]$ExePath = "C:\Watchdog\watchdog.exe",
    [string]$ServiceName = "watchdog",
    [string]$ConfigPath = ""
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $ExePath)) {
    Write-Error "未找到看门狗程序：$ExePath（请先构建并拷贝，或用 -ExePath 指定实际路径）"
    exit 1
}

# 服务 binPath：exe 路径 + run 子命令 + 可选配置路径
$binPath = "`"$ExePath`" run"
if ($ConfigPath) {
    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        Write-Error "未找到配置文件：$ConfigPath"
        exit 1
    }
    $binPath += " --config `"$ConfigPath`""
}

# 已存在则先提示
$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "服务 $ServiceName 已存在，先删除旧注册……"
    sc.exe stop $ServiceName | Out-Null
    sc.exe delete $ServiceName | Out-Null
    Start-Sleep -Seconds 2
}

Write-Host "==> 创建服务 $ServiceName (binPath: $binPath)"
sc.exe create $ServiceName binPath= $binPath start= auto DisplayName= "Application Watchdog"
if ($LASTEXITCODE -ne 0) { Write-Error "sc.exe create 失败 (exit $LASTEXITCODE)"; exit 1 }

# 服务描述
sc.exe description $ServiceName "应用看门狗：自动监测并拉起配置中的程序（exe/bat/jar/ps1）" | Out-Null

# 看门狗自身失败后自动重启：24 小时内最多 3 次，每次间隔 60 秒
sc.exe failure $ServiceName reset= 86400 actions= restart/60000/restart/60000/restart/60000 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Error "sc.exe failure 失败 (exit $LASTEXITCODE)"; exit 1 }

Write-Host "==> 启动服务 $ServiceName ..."
sc.exe start $ServiceName
if ($LASTEXITCODE -ne 0) { Write-Error "sc.exe start 失败 (exit $LASTEXITCODE)"; exit 1 }

Write-Host "==> 完成。"
Write-Host "    查看状态: sc.exe query $ServiceName"
Write-Host "    查看日志: $((Split-Path $ExePath) -join '\logs\watchdog.log')"
