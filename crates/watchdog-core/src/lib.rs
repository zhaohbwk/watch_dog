//! 应用看门狗共享核心库。
//!
//! 供服务端 CLI（watchdog）与图形配置工具（watchdog-gui）共用：
//! 配置解析与保存、进程拉起、监控循环、文件日志、Windows 服务集成。

pub mod config;
pub mod launcher;
pub mod logger;
pub mod monitor;
pub mod status;
pub mod util;

#[cfg(windows)]
pub mod service;

pub use config::EffectiveProgram;

/// 构建版本号：编译时刻的本地时间，格式「年.月日.时分」（如 `2026.1123.1114`），
/// 由 watchdog-core 的 build.rs 在编译期生成；构建脚本未运行时（如部分 IDE 索引）显示 dev。
pub const BUILD_VERSION: &str = match option_env!("WATCHDOG_BUILD_VERSION") {
    Some(v) => v,
    None => "dev",
};

/// Windows 服务名（服务端与 GUI 共用）
pub const SERVICE_NAME: &str = "watchdog";
