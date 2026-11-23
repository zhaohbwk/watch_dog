//! Windows 服务集成：以服务方式运行看门狗，开机自启、响应 SCM 停止。

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

use crate::config::Config;
use crate::logger::Logger;
use crate::monitor;
use crate::util::{default_config_path, exe_dir, parse_args};
use crate::SERVICE_NAME;

// 生成 SCM 需要的 extern "system" 入口，并把系统传入的参数转交给 service_main
define_windows_service!(ffi_service_main, service_main);

/// 服务分发入口：阻塞直到服务停止。
pub fn run_service() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(arguments: Vec<OsString>) {
    // arguments 为 SCM 传入的启动参数（安装脚本 binPath 中 exe 之后的部分）
    let args: Vec<String> = arguments
        .iter()
        .filter_map(|a| a.to_str().map(str::to_string))
        .collect();
    let parsed = parse_args(&args);

    if let Err(e) = run(parsed.config) {
        // 兜底日志：直接写 exe 目录，避免服务静默失败
        if let Ok(logger) = Logger::new(exe_dir().join("logs").join("watchdog.log"), false) {
            logger.error(&format!("服务运行失败：{e}"));
        }
    }
}

fn run(config_path: Option<PathBuf>) -> Result<(), String> {
    // 1) 注册控制处理器：收到 STOP / 系统关机 → 置停止标志
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = Arc::clone(&stop);
    let handler = move |event: ServiceControl| -> ServiceControlHandlerResult {
        match event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_for_handler.store(true, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, handler)
        .map_err(|e| format!("注册服务控制处理器失败：{e}"))?;

    // 2) 尽快上报 RUNNING，避免被 SCM 判定启动超时
    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });

    // 3) 加载配置并初始化日志
    let config_path = config_path.unwrap_or_else(default_config_path);
    let config = Config::load(&config_path)?;
    let log_path = config
        .log_file
        .clone()
        .unwrap_or_else(|| exe_dir().join("logs").join("watchdog.log"));
    let logger = Logger::new(log_path, false)?;

    // 4) 监控主循环（stop 置位后返回；config_path 用于定位暂停标记）
    monitor::run_loop(&config, &config_path, &logger, &stop)?;
    logger.info("服务停止");

    // 5) 上报 STOPPED（看门狗退出，已拉起的子进程保持运行）
    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });
    Ok(())
}
