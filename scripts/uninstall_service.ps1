#Requires -RunAsAdministrator
<#
.SYNOPSIS
    停止并删除应用看门狗 Windows 服务

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File uninstall_service.ps1

.NOTES
    卸载只删除看门狗服务本身；看门狗此前拉起的被监控程序不会被终止。
#>
param(
    [string]$ServiceName = "watchdog"
)

$ErrorActionPreference = "Stop"

$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if (-not $existing) {
    Write-Host "服务 $ServiceName 不存在，无需卸载。"
    exit 0
}

if ($existing.Status -ne "Stopped") {
    Write-Host "==> 停止服务 $ServiceName ..."
    sc.exe stop $ServiceName
    if ($LASTEXITCODE -ne 0) { Write-Error "sc.exe stop 失败 (exit $LASTEXITCODE)"; exit 1 }
}

Write-Host "==> 删除服务 $ServiceName ..."
sc.exe delete $ServiceName
if ($LASTEXITCODE -ne 0) { Write-Error "sc.exe delete 失败 (exit $LASTEXITCODE)"; exit 1 }

Write-Host "==> 完成。"
