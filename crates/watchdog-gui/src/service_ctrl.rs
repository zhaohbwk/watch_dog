//! Windows 服务管理：查询 / 安装 / 启动 / 停止 / 重启 / 卸载，以及管理员权限检测。
//!
//! Windows 上全部通过 windows-service / windows-sys 直接调用 SCM，无 shell 调用；
//! 其它平台提供返回错误的桩函数，保证界面代码可跨平台编译。

use std::path::Path;

#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use watchdog_core::SERVICE_NAME;

/// 服务状态（简化，供界面显示）
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvcState {
    NotInstalled,
    Stopped,
    StartPending,
    Running,
    StopPending,
    Paused,
}

impl SvcState {
    pub fn label(self) -> &'static str {
        match self {
            SvcState::NotInstalled => "未安装",
            SvcState::Stopped => "已停止",
            SvcState::StartPending => "正在启动",
            SvcState::Running => "运行中",
            SvcState::StopPending => "正在停止",
            SvcState::Paused => "已暂停",
        }
    }
}

// ==================== Windows 实现 ====================

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions,
        ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    /// ERROR_SERVICE_DOES_NOT_EXIST
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;

    fn from_status(status: &ServiceStatus) -> SvcState {
        match status.current_state {
            ServiceState::Stopped => SvcState::Stopped,
            ServiceState::StartPending => SvcState::StartPending,
            ServiceState::Running => SvcState::Running,
            ServiceState::StopPending => SvcState::StopPending,
            ServiceState::Paused | ServiceState::PausePending | ServiceState::ContinuePending => {
                SvcState::Paused
            }
        }
    }

    fn is_not_installed(e: &windows_service::Error) -> bool {
        matches!(e, windows_service::Error::Winapi(io) if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST))
    }

    fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
        ServiceManager::local_computer(None::<&str>, access)
            .map_err(|e| format!("打开服务管理器失败：{e}"))
    }

    fn open_for(access: ServiceAccess) -> Result<windows_service::service::Service, String> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        m.open_service(SERVICE_NAME, access)
            .map_err(|e| format!("打开服务失败：{e}"))
    }

    /// 等待服务进入停止状态（最多 30 秒）
    fn wait_stopped(svc: &windows_service::service::Service) -> Result<(), String> {
        for _ in 0..150 {
            match svc.query_status() {
                Ok(s) if s.current_state == ServiceState::Stopped => return Ok(()),
                Ok(_) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => return Err(format!("查询停止状态失败：{e}")),
            }
        }
        Err("等待服务停止超时（30 秒）".into())
    }

    /// 查询看门狗服务状态；`Ok(None)` 表示未安装。
    pub fn query() -> Result<Option<SvcState>, String> {
        let m = manager(ServiceManagerAccess::CONNECT)?;
        match m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
            Ok(svc) => svc
                .query_status()
                .map(|s| Some(from_status(&s)))
                .map_err(|e| format!("查询服务状态失败：{e}")),
            Err(e) if is_not_installed(&e) => Ok(None),
            Err(e) => Err(format!("打开服务失败：{e}")),
        }
    }

    pub fn start_service() -> Result<(), String> {
        let svc = open_for(ServiceAccess::START)?;
        svc.start(&[] as &[&str]).map_err(|e| format!("启动服务失败：{e}"))
    }

    pub fn stop_service() -> Result<(), String> {
        let svc = open_for(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?;
        svc.stop().map_err(|e| format!("停止服务失败：{e}"))?;
        wait_stopped(&svc)
    }

    pub fn restart_service() -> Result<(), String> {
        if let Some(state) = query()? {
            if state != SvcState::Stopped {
                stop_service()?;
            }
        }
        start_service()
    }

    pub fn delete_service() -> Result<(), String> {
        let svc = open_for(ServiceAccess::STOP | ServiceAccess::DELETE)?;
        let _ = svc.stop();
        svc.delete().map_err(|e| format!("删除服务失败：{e}"))
    }

    /// 安装并启动服务；`exe` 为 watchdog.exe 路径，`config` 为配置文件路径。
    pub fn install(exe: &Path, config: &Path) -> Result<(), String> {
        let m = manager(ServiceManagerAccess::CREATE_SERVICE | ServiceManagerAccess::CONNECT)?;

        // 已存在则先卸载旧的（覆盖安装）
        if let Ok(old) = m.open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE,
        ) {
            let _ = old.stop();
            let _ = old.delete();
            for _ in 0..150 {
                if matches!(query(), Ok(None) | Ok(Some(SvcState::Stopped))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        let info = ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: "Application Watchdog".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.to_path_buf(),
            launch_arguments: vec![
                "run".into(),
                "--config".into(),
                config.as_os_str().to_os_string(),
            ],
            dependencies: Vec::new(),
            account_name: None,
            account_password: None,
        };
        let svc = m
            .create_service(
                &info,
                ServiceAccess::START | ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_STATUS,
            )
            .map_err(|e| format!("创建服务失败：{e}"))?;

        // 看门狗自身崩溃后自动重启：24 小时内最多 3 次，间隔 60 秒（与 install_service.ps1 一致）
        let _ = svc.update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(60),
                };
                3
            ]),
        });
        let _ = svc.set_description("应用看门狗：自动监测并拉起配置中的程序（exe/bat/jar/ps1）");

        svc.start(&[] as &[&str]).map_err(|e| format!("启动服务失败：{e}"))?;
        Ok(())
    }

    /// 当前进程是否以管理员权限运行
    pub fn is_elevated() -> bool {
        use std::mem::size_of;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
            let mut returned: u32 = 0;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut core::ffi::c_void,
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            );
            CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }

    /// 请求以管理员身份重新启动本程序；返回是否成功发起。
    pub fn relaunch_as_admin() -> bool {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let exe_wide = to_wide(&exe.to_string_lossy());
        let op_wide = to_wide("runas");
        unsafe {
            let result = ShellExecuteW(
                std::ptr::null_mut(),
                op_wide.as_ptr(),
                exe_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
            result as isize > 32
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(windows)]
pub use imp::*;

// ==================== 其它平台桩 ====================

#[cfg(not(windows))]
pub fn query() -> Result<Option<SvcState>, String> {
    Err("服务管理仅在 Windows 上可用".into())
}

#[cfg(not(windows))]
pub fn start_service() -> Result<(), String> {
    Err("服务管理仅在 Windows 上可用".into())
}

#[cfg(not(windows))]
pub fn stop_service() -> Result<(), String> {
    Err("服务管理仅在 Windows 上可用".into())
}

#[cfg(not(windows))]
pub fn restart_service() -> Result<(), String> {
    Err("服务管理仅在 Windows 上可用".into())
}

#[cfg(not(windows))]
pub fn delete_service() -> Result<(), String> {
    Err("服务管理仅在 Windows 上可用".into())
}

#[cfg(not(windows))]
pub fn install(_exe: &Path, _config: &Path) -> Result<(), String> {
    Err("服务管理仅在 Windows 上可用".into())
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(not(windows))]
pub fn relaunch_as_admin() -> bool {
    false
}
