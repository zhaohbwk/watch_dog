//! 应用看门狗服务端入口：以 Windows 服务方式运行，监测并自动拉起配置的程序。
//!
//! 用法见 USAGE 常量与 README.md。

use std::env;
use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::AtomicBool;

use watchdog_core::config::Config;
use watchdog_core::logger::Logger;
use watchdog_core::monitor;
use watchdog_core::util::{
    default_config_path, parse_args, port_owners, process_ports, resolve_log_path,
};

const USAGE: &str = r#"应用看门狗 (Application Watchdog)

用法:
  watchdog version                       显示版本号
  watchdog run [--config <file>]         以 Windows 服务方式运行（由服务控制管理器调用）
  watchdog foreground [--config <file>]  在前台控制台运行（调试用，Ctrl+C 退出）
  watchdog validate [--config <file>]    校验配置文件并打印生效的监控规则
  watchdog port <端口号>                  查询监听该端口的进程 PID 与进程名
  watchdog proc <进程名>                  查询该进程名监听的所有端口号
  watchdog --version                     显示版本号

配置文件名默认为可执行文件同目录下的 watchdog.toml。
查询命令说明：proc 的进程名忽略大小写与 .exe 后缀（java 可匹配 java.exe）；
两项查询均覆盖 IPv4 与 IPv6，未找到结果时以退出码 1 结束。
"#;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprint!("{USAGE}");
        eprintln!("版本号: {}", watchdog_core::BUILD_VERSION);
        return;
    }

    let parsed = parse_args(&args[1..]);
    match parsed.command.as_str() {
        "validate" => cmd_validate(parsed.config),
        "foreground" => cmd_foreground(parsed.config),
        "run" => cmd_run(),
        "version" => cmd_version(),
        "port" => cmd_port(parsed.arg.as_deref()),
        "proc" => cmd_proc(parsed.arg.as_deref()),
        _ => {
            eprint!("{USAGE}");
            eprintln!("版本号: {}", watchdog_core::BUILD_VERSION);
        }
    }
}

/// 显示版本号（格式「年.月日.时分」，即本次编译时间）
fn cmd_version() {
    println!(
        "应用看门狗 (Application Watchdog) v{}",
        watchdog_core::BUILD_VERSION
    );
}

/// 根据端口查询监听进程的 PID 与进程名
fn cmd_port(arg: Option<&str>) {
    let text = match arg {
        Some(t) => t,
        None => {
            eprintln!("缺少端口号参数。用法：watchdog port <端口号>");
            exit(1);
        }
    };
    let port: u16 = match text.parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("端口号无效：{text}（应为 0~65535 的数字）");
            exit(1);
        }
    };
    let owners = port_owners(port);
    if owners.is_empty() {
        println!("端口 {port} 当前没有监听进程");
        exit(1);
    }
    println!("端口 {port} 的监听进程：");
    for owner in owners {
        let name = if owner.name.is_empty() {
            "（进程已退出或无权限读取名称）"
        } else {
            &owner.name
        };
        println!("  PID {:<10}{}", owner.pid, name);
    }
}

/// 根据进程名查询其监听的所有端口号
fn cmd_proc(arg: Option<&str>) {
    let name = match arg {
        Some(n) => n,
        None => {
            eprintln!("缺少进程名参数。用法：watchdog proc <进程名>（如 java 或 java.exe）");
            exit(1);
        }
    };
    let groups = process_ports(name);
    if groups.is_empty() {
        println!("未找到进程「{name}」监听的端口");
        exit(1);
    }
    println!("进程 {name} 的监听端口：");
    for group in groups {
        let ports = group
            .ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join("  ");
        println!("  PID {:<10}{}", group.pid, ports);
    }
}

/// 校验配置并打印生效规则
fn cmd_validate(config_path: Option<PathBuf>) {
    let path = config_path.unwrap_or_else(default_config_path);
    match Config::load(&path) {
        Ok(config) => {
            println!("配置校验通过：{}", path.display());
            println!("版本号：{}", watchdog_core::BUILD_VERSION);
            println!("检测间隔：{} 秒", config.check_interval_secs);
            println!("日志文件：{}", resolve_log_path(&config).display());
            let effective = config.effective_programs();
            let skipped = effective.iter().filter(|p| p.skip).count();
            if skipped > 0 {
                println!(
                    "共 {} 个程序：监控 {} 个，跳过 {} 个：",
                    effective.len(),
                    effective.len() - skipped,
                    skipped
                );
            } else {
                println!("共监控 {} 个程序：", effective.len());
            }
            for prog in effective {
                println!("  {}", prog.describe());
            }
        }
        Err(e) => {
            eprintln!("配置校验失败：{e}");
            exit(1);
        }
    }
}

/// 前台控制台运行（调试用）
fn cmd_foreground(config_path: Option<PathBuf>) {
    let path = config_path.unwrap_or_else(default_config_path);
    let config = match Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置加载失败：{e}");
            exit(1);
        }
    };
    let logger = match Logger::new(resolve_log_path(&config), true) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("初始化日志失败：{e}");
            exit(1);
        }
    };
    eprintln!("前台运行中（Ctrl+C 退出）……");
    let stop = AtomicBool::new(false);
    if let Err(e) = monitor::run_loop(&config, &path, &logger, &stop) {
        logger.error(&format!("监控循环异常退出：{e}"));
        exit(1);
    }
}

/// 服务模式入口（由 SCM 调用）
#[cfg(windows)]
fn cmd_run() {
    if let Err(e) = watchdog_core::service::run_service() {
        eprintln!("服务运行失败：{e}");
        exit(1);
    }
}

#[cfg(not(windows))]
fn cmd_run() {
    eprintln!("run 子命令仅在 Windows 上可用（需由服务控制管理器调用）");
}
