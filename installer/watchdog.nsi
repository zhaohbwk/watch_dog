; ======================================================================
; 应用看门狗（Application Watchdog）Windows 安装程序 — NSIS 脚本
;
; 编译入口：installer\build_installer.ps1（自动传入版本号定义）
;   makensis /DSETUP_VERSION_RAW=2026.1123.1931 /DSETUP_VERSION=2026.11.23.1931 watchdog.nsi
;
; 注意：本文件必须保存为 UTF-8 with BOM（makensis 以 BOM 识别 UTF-8，
;       否则会按 ANSI 解析导致中文乱码）。build_installer.ps1 会自动补 BOM。
;
; 行为要点：
;   - 升级安装时若已有 watchdog.toml，弹窗询问是否覆盖（覆盖前备份 .bak）
;   - 升级前自动停止 watchdog 服务并在装完后恢复；自动关闭 watchdog-gui.exe
;   - 可勾选安装并启动 Windows 服务（开机自启 + 崩溃自动重启）
;   - 卸载时保留 watchdog.toml / *.bak / logs\ / *.pause 等运行数据
; ======================================================================

Unicode true

!define APP_NAME "应用看门狗"
!define APP_EN "Application Watchdog"
!define SERVICE_NAME "watchdog"
!define EXE_NAME "watchdog.exe"
!define GUI_NAME "watchdog-gui.exe"
!define CONFIG_NAME "watchdog.toml"
!define EXAMPLE_NAME "watchdog.example.toml"

; 版本号由 build_installer.ps1 传入：
;   SETUP_VERSION_RAW = 2026.1123.1931（安装包文件名、显示版本）
;   SETUP_VERSION     = 2026.11.23.1931（NSIS 版本资源要求 X.X.X.X）
!ifndef SETUP_VERSION_RAW
  !define SETUP_VERSION_RAW "dev"
!endif
!ifndef SETUP_VERSION
  !define SETUP_VERSION "0.0.0.0"
!endif

Name "${APP_NAME}（${APP_EN}） ${SETUP_VERSION_RAW}"
!ifdef TESTBUILD
; 测试构建：不要求管理员，便于无人值守冒烟测试
;（HKLM 注册表写入与 sc.exe 服务操作会静默失败，不影响文件/配置逻辑验证）
OutFile "dist\watchdog-setup-TEST-${SETUP_VERSION_RAW}.exe"
RequestExecutionLevel user
!else
OutFile "dist\watchdog-setup-${SETUP_VERSION_RAW}.exe"
RequestExecutionLevel admin
!endif
InstallDir "C:\Watchdog"
; 已安装过时默认使用上次安装目录
InstallDirRegKey HKLM "Software\${APP_EN}" "InstallDir"
SetCompressor /SOLID lzma

VIProductVersion "${SETUP_VERSION}"
VIAddVersionKey /LANG=2052 "ProductName" "${APP_NAME}（${APP_EN}）"
VIAddVersionKey /LANG=2052 "FileDescription" "${APP_NAME} 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${SETUP_VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${SETUP_VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "Application Watchdog"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "FileFunc.nsh"

; ---------------- 运行期变量 ----------------
Var OverwriteConfig    ; 1=覆盖现有配置 0=保留
Var ForceOverwrite     ; 1=命令行 /overwriteconfig 强制覆盖（静默模式生效）
Var CreateDesktopIcon  ; 1=创建桌面快捷方式
Var InstallService     ; 1=安装并启动服务
Var ServiceWasRunning  ; 1=安装前服务正在运行（装完恢复启动）
Var ServiceRunning     ; 临时：服务是否正在运行（0=是）
Var GuiRunning         ; 临时：GUI 是否正在运行（0=是）

; ---------------- 品牌视觉（素材见 assets/logo/，位图由 installer/tools/gen_brand_assets.py 生成） ----------------
!define MUI_ICON "..\assets\logo\watchdog.ico"               ; 安装程序 exe 图标
!define MUI_UNICON "..\assets\logo\watchdog.ico"             ; 卸载程序 exe 图标
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "assets\header.bmp"           ; 向导页眉图（150x57）
!define MUI_WELCOMEFINISHPAGE_BITMAP "assets\sidebar.bmp"    ; 欢迎/完成页侧边图（164x314）

; ---------------- 页面 ----------------
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
Page custom OptionsPageCreate OptionsPageLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "运行图形配置工具（watchdog-gui.exe）"
!define MUI_FINISHPAGE_RUN_FUNCTION "LaunchGui"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"

; ---------------- 初始化 ----------------
Function .onInit
  StrCpy $OverwriteConfig 1
  StrCpy $ForceOverwrite 0
  StrCpy $CreateDesktopIcon 1
  StrCpy $InstallService 1
  StrCpy $ServiceWasRunning 0

  ; 静默安装开关：
  ;   /noservice        跳过服务注册（如测试安装包时）
  ;   /overwriteconfig  强制覆盖已有配置（静默模式默认保留，需覆盖时配合使用）
  ;   /nodesktop        不创建桌面快捷方式
  ${GetParameters} $R0
  ${GetOptions} $R0 "/noservice" $R1
  ${IfNot} ${Errors}
    StrCpy $InstallService 0
  ${EndIf}
  ${GetOptions} $R0 "/overwriteconfig" $R1
  ${IfNot} ${Errors}
    StrCpy $ForceOverwrite 1
  ${EndIf}
  ${GetOptions} $R0 "/nodesktop" $R1
  ${IfNot} ${Errors}
    StrCpy $CreateDesktopIcon 0
  ${EndIf}
FunctionEnd

; ---------------- 附加任务选项页 ----------------
Var hOptionsDlg
Var chkDesktop
Var chkService

Function OptionsPageCreate
  !insertmacro MUI_HEADER_TEXT "附加任务" "选择要执行的附加任务"
  nsDialogs::Create 1018
  Pop $hOptionsDlg
  ${If} $hOptionsDlg == error
    Abort
  ${EndIf}

  ${NSD_CreateCheckbox} 0 12u 100% 10u "创建桌面快捷方式"
  Pop $chkDesktop
  ${NSD_SetState} $chkDesktop ${BST_CHECKED}

  ${NSD_CreateCheckbox} 0 32u 100% 10u "安装并启动 Windows 服务（开机自启，看门狗崩溃后自动重启）"
  Pop $chkService
  ${NSD_SetState} $chkService ${BST_CHECKED}

  ${NSD_CreateLabel} 0 52u 100% 24u "说明：服务以 SYSTEM 账户后台运行，监控并自动拉起配置中的程序；$\r$\n升级安装时会自动停止运行中的服务，安装完成后恢复启动。"
  Pop $0

  nsDialogs::Show
FunctionEnd

Function OptionsPageLeave
  ${NSD_GetState} $chkDesktop $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $CreateDesktopIcon 1
  ${Else}
    StrCpy $CreateDesktopIcon 0
  ${EndIf}
  ${NSD_GetState} $chkService $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $InstallService 1
  ${Else}
    StrCpy $InstallService 0
  ${EndIf}
FunctionEnd

; ---------------- 服务 / 进程检测辅助 ----------------
; 结果写入 $ServiceRunning：0=运行中，非 0=未运行或服务不存在
Function CheckServiceRunning
  nsExec::Exec 'cmd /c sc.exe query ${SERVICE_NAME} 2>nul | find "RUNNING" >nul'
  Pop $ServiceRunning
FunctionEnd

; 结果写入 $GuiRunning：0=运行中，非 0=未运行
Function CheckGuiRunning
  nsExec::Exec 'cmd /c tasklist /FI "IMAGENAME eq ${GUI_NAME}" 2>nul | find /I "${GUI_NAME}" >nul'
  Pop $GuiRunning
FunctionEnd

; 安装前：关闭运行中的 watchdog-gui.exe（先温和关闭，超时后强杀）
Function CloseGuiIfRunning
  Call CheckGuiRunning
  ${If} $GuiRunning != 0
    Return
  ${EndIf}
  ${IfNot} ${Silent}
    MessageBox MB_OK|MB_ICONINFORMATION \
      "检测到图形配置工具（${GUI_NAME}）正在运行，将自动关闭它以完成安装/升级。"
  ${EndIf}
  DetailPrint "关闭图形配置工具 ${GUI_NAME}..."
  nsExec::Exec 'taskkill /IM ${GUI_NAME}'
  Pop $0
  StrCpy $1 0
  ${While} $1 < 10
    Call CheckGuiRunning
    ${If} $GuiRunning != 0
      ${Break}
    ${EndIf}
    Sleep 500
    IntOp $1 $1 + 1
  ${EndWhile}
  Call CheckGuiRunning
  ${If} $GuiRunning == 0
    nsExec::Exec 'taskkill /F /IM ${GUI_NAME}'
    Pop $0
    Sleep 1000
  ${EndIf}
FunctionEnd

; 安装前：停止运行中的 watchdog 服务（记录状态，装完恢复）
Function StopServiceIfRunning
  nsExec::Exec 'sc.exe query ${SERVICE_NAME}'
  Pop $0
  ${If} $0 != 0
    StrCpy $ServiceWasRunning 0
    Return
  ${EndIf}
  Call CheckServiceRunning
  ${If} $ServiceRunning != 0
    Return          ; 已注册但未在运行，保持原状态
  ${EndIf}
  StrCpy $ServiceWasRunning 1
  DetailPrint "停止看门狗服务 ${SERVICE_NAME}..."
  nsExec::Exec 'sc.exe stop ${SERVICE_NAME}'
  Pop $0
  ; 等待服务真正停止（最多约 20 秒），避免文件占用
  StrCpy $1 0
  ${While} $1 < 40
    Call CheckServiceRunning
    ${If} $ServiceRunning != 0
      ${Break}
    ${EndIf}
    Sleep 500
    IntOp $1 $1 + 1
  ${EndWhile}
FunctionEnd

; ---------------- 配置文件覆盖提示（核心需求） ----------------
; 结果写入 $OverwriteConfig：1=覆盖（默认） 0=保留
; - 配置不存在 → 直接生成新配置（保持 1）
; - 配置已存在且为静默安装 → 保留（0）
; - 配置已存在且为交互安装 → 弹窗询问：是=覆盖，否=保留（默认按钮为否）
Function AskConfigOverwrite
  ${If} ${FileExists} "$INSTDIR\${CONFIG_NAME}"
    ${If} $ForceOverwrite == 1
      StrCpy $OverwriteConfig 1
    ${ElseIf} ${Silent}
      StrCpy $OverwriteConfig 0
    ${Else}
      MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 \
        "检测到已有配置文件：$\r$\n  $INSTDIR\${CONFIG_NAME}$\r$\n$\r$\n\
        选择「是(Y)」：覆盖为新版默认配置，原配置自动备份为 ${CONFIG_NAME}.bak$\r$\n\
        选择「否(N)」：保留现有配置（新版配置样例为 ${EXAMPLE_NAME}）$\r$\n$\r$\n\
        是否覆盖现有配置文件？" \
        IDYES +2
      StrCpy $OverwriteConfig 0
    ${EndIf}
  ${EndIf}
FunctionEnd

; 按用户选择处理配置文件（在解压文件之后调用）
Function ApplyConfigDecision
  ${If} $OverwriteConfig == 1
    ${If} ${FileExists} "$INSTDIR\${CONFIG_NAME}"
      DetailPrint "备份原配置：${CONFIG_NAME} → ${CONFIG_NAME}.bak"
      CopyFiles /SILENT "$INSTDIR\${CONFIG_NAME}" "$INSTDIR\${CONFIG_NAME}.bak"
      Delete "$INSTDIR\${CONFIG_NAME}"
    ${EndIf}
    CopyFiles /SILENT "$INSTDIR\${EXAMPLE_NAME}" "$INSTDIR\${CONFIG_NAME}"
    DetailPrint "已写入新配置 ${CONFIG_NAME}"
  ${Else}
    DetailPrint "保留现有配置 ${CONFIG_NAME}（新版样例：${EXAMPLE_NAME}）"
  ${EndIf}
FunctionEnd

; ---------------- 服务安装（等效 scripts\install_service.ps1） ----------------
Function InstallAndStartService
  DetailPrint "安装 Windows 服务 ${SERVICE_NAME}（开机自启 + 崩溃自动重启）..."
  nsExec::Exec 'sc.exe query ${SERVICE_NAME}'
  Pop $0
  ${If} $0 != 0
    nsExec::ExecToLog 'sc.exe create ${SERVICE_NAME} binPath= "\"$INSTDIR\${EXE_NAME}\" run" start= auto DisplayName= "${APP_EN}"'
    Pop $0
    nsExec::ExecToLog 'sc.exe description ${SERVICE_NAME} "应用看门狗：自动监测并拉起配置中的程序（exe/bat/jar/ps1）"'
    Pop $0
    ; 看门狗自身失败后自动重启：24 小时内最多 3 次，每次间隔 60 秒
    nsExec::ExecToLog 'sc.exe failure ${SERVICE_NAME} reset= 86400 actions= restart/60000/restart/60000/restart/60000'
    Pop $0
  ${EndIf}
  DetailPrint "启动看门狗服务 ${SERVICE_NAME}..."
  nsExec::Exec 'sc.exe start ${SERVICE_NAME}'
  Pop $0
  ${If} $0 == 0
    DetailPrint "看门狗服务已启动。"
  ${Else}
    DetailPrint "服务启动失败（exit $0），可稍后在 watchdog-gui.exe 中重试。"
  ${EndIf}
FunctionEnd

; ---------------- 主安装段 ----------------
Section ""
  Call CloseGuiIfRunning
  Call StopServiceIfRunning
  Call AskConfigOverwrite

  SetOutPath "$INSTDIR"
  File "..\target\release\${EXE_NAME}"
  File "..\target\release\${GUI_NAME}"
  File "..\${EXAMPLE_NAME}"
  File "/oname=README.md" "..\README.md"
  SetOutPath "$INSTDIR\scripts"
  File "..\scripts\install_service.ps1"
  File "..\scripts\uninstall_service.ps1"
  SetOutPath "$INSTDIR"

  Call ApplyConfigDecision

  ; 快捷方式
  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortCut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${GUI_NAME}"
  CreateShortCut "$SMPROGRAMS\${APP_NAME}\卸载${APP_NAME}.lnk" "$INSTDIR\Uninstall.exe"
  ${If} $CreateDesktopIcon == 1
    CreateShortCut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${GUI_NAME}"
  ${EndIf}

  ; 注册表：安装目录记忆 + 控制面板「应用」卸载项
  WriteRegStr HKLM "Software\${APP_EN}" "InstallDir" $INSTDIR
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "DisplayName" "${APP_NAME}（${APP_EN}）"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "DisplayVersion" "${SETUP_VERSION_RAW}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "Publisher" "${APP_EN}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "DisplayIcon" "$INSTDIR\${GUI_NAME}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "EstimatedSize" 16384
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}" "NoRepair" 1

  ; 服务：勾选则安装并启动；未勾选但升级前服务在运行 → 恢复启动
  ${If} $InstallService == 1
    Call InstallAndStartService
  ${ElseIf} $ServiceWasRunning == 1
    DetailPrint "恢复启动看门狗服务 ${SERVICE_NAME}..."
    nsExec::Exec 'sc.exe start ${SERVICE_NAME}'
    Pop $0
  ${EndIf}

  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

; ---------------- 卸载 ----------------
Section "Uninstall"
  DetailPrint "停止并删除 Windows 服务 ${SERVICE_NAME}..."
  nsExec::Exec 'sc.exe stop ${SERVICE_NAME}'
  Pop $0
  Sleep 2000
  nsExec::Exec 'sc.exe delete ${SERVICE_NAME}'
  Pop $0

  nsExec::Exec 'taskkill /F /IM ${GUI_NAME}'
  Pop $0
  Sleep 500

  Delete "$INSTDIR\${EXE_NAME}"
  Delete "$INSTDIR\${GUI_NAME}"
  Delete "$INSTDIR\${EXAMPLE_NAME}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$INSTDIR\scripts\install_service.ps1"
  Delete "$INSTDIR\scripts\uninstall_service.ps1"
  RMDir "$INSTDIR\scripts"
  RMDir "$INSTDIR\logs"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\卸载${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_EN}"
  DeleteRegKey /ifempty HKLM "Software\${APP_EN}"

  ; 保留用户运行数据：watchdog.toml / watchdog.toml.bak / logs\ / *.pause
  DetailPrint "配置与运行数据已保留（${CONFIG_NAME}、logs\ 等），如需彻底清理请手动删除。"
SectionEnd

Function LaunchGui
  Exec '"$INSTDIR\${GUI_NAME}"'
FunctionEnd
