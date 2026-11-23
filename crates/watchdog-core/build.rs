//! 构建脚本：生成本次编译的版本号并注入环境变量 `WATCHDOG_BUILD_VERSION`。
//!
//! 版本号取编译时刻的本地时间，格式为「年.月日.时分」，如 `2026.1123.1114`。
//! Cargo 在包内任一文件变化重新编译时会重跑本脚本，因此版本号随实际构建自动更新，
//! 可用于区分「手上跑的是哪次构建的 exe」。

use time::macros::format_description;

fn main() {
    let fmt = format_description!("[year].[month][day].[hour][minute]");
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let version = now.format(&fmt).unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=WATCHDOG_BUILD_VERSION={version}");
    println!("cargo:rerun-if-changed=build.rs");
}
