//! 通用工具：命令行解析、可执行文件目录、默认配置/日志路径、端口探测、进程/端口互查。

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::config::Config;

/// 端口探测的连接超时：本地端口通常瞬时返回，远程主机不可达时最多阻塞该时长
const PORT_CHECK_TIMEOUT: Duration = Duration::from_millis(1000);

/// 可执行文件所在目录
pub fn exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 默认配置文件路径：exe 同目录 watchdog.toml
pub fn default_config_path() -> PathBuf {
    exe_dir().join("watchdog.toml")
}

/// 日志路径：优先取配置，否则 exe 同目录 logs\watchdog.log
pub fn resolve_log_path(config: &Config) -> PathBuf {
    config
        .log_file
        .clone()
        .unwrap_or_else(|| exe_dir().join("logs").join("watchdog.log"))
}

/// 程序输出日志路径：与看门狗日志同目录，命名为 `<name>.log`。
///
/// `name` 中的 Windows 文件名非法字符替换为 `_`（name 经配置校验非空，
/// 全为非法字符时也会得到下划线，不会产生空文件名）。
pub fn program_log_path(base_log: &Path, name: &str) -> PathBuf {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let file = format!("{}.log", if sanitized.is_empty() { "_" } else { &sanitized });
    base_log
        .parent()
        .map(|dir| dir.join(&file))
        .unwrap_or_else(|| PathBuf::from(file))
}

/// TCP 监听表项：本地端口与归属进程 PID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenEntry {
    /// 本地监听端口号
    pub port: u16,
    /// 归属进程 PID（已过滤 PID ≤ 4 的内核/System 项）
    pub pid: u32,
}

/// 枚举当前所有 TCP 监听表项（IPv4 + IPv6）。
///
/// 通过 `GetExtendedTcpTable` 枚举带 PID 归属的 LISTENING 套接字，
/// 供「按端口查进程」「按进程名查端口」两类查询与端口检测程序的精确停止使用。
/// 查询失败时返回空列表（调用方回退到按检测规则的匹配）。
#[cfg(windows)]
pub fn listening_tcp_entries() -> Vec<ListenEntry> {
    let mut entries = listening_entries_v4();
    entries.extend(listening_entries_v6());
    entries
}

#[cfg(not(windows))]
pub fn listening_tcp_entries() -> Vec<ListenEntry> {
    Vec::new()
}

/// 查询当前监听指定 TCP 端口的进程 PID 列表（IPv4 + IPv6，按 PID 去重升序）。
///
/// 用于端口检测程序的精确停止：只终止真正监听该端口的进程，
/// 不误杀镜像名/命令行相同的其它程序。
/// 查询失败或无监听进程时返回空列表（调用方回退到按检测规则的匹配）。
pub fn pids_listening_port(port: u16) -> Vec<u32> {
    let mut pids: Vec<u32> = listening_tcp_entries()
        .into_iter()
        .filter(|e| e.port == port)
        .map(|e| e.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// 端口占用查询结果：监听进程的 PID 与镜像名
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortOwner {
    /// 监听进程 PID
    pub pid: u32,
    /// 进程镜像名（如 java.exe）；进程已退出或读取不到时为空串
    pub name: String,
}

/// 根据端口查询监听进程的名称与 PID（IPv4 + IPv6，按 PID 去重升序）。
///
/// 无监听进程时返回空列表。
pub fn port_owners(port: u16) -> Vec<PortOwner> {
    let pids = pids_listening_port(port);
    let names = pid_image_names(&pids);
    pids.into_iter()
        .map(|pid| PortOwner {
            name: names.get(&pid).cloned().unwrap_or_default(),
            pid,
        })
        .collect()
}

/// 进程监听端口查询结果：某个进程 PID 及其监听的所有端口号
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPorts {
    /// 进程 PID
    pub pid: u32,
    /// 进程镜像名
    pub name: String,
    /// 监听的端口号（去重升序，IPv4 / IPv6 合并）
    pub ports: Vec<u16>,
}

/// 根据进程名查询其监听的所有 TCP 端口号（IPv4 + IPv6，按 PID 升序）。
///
/// 进程名忽略大小写与 .exe 后缀（`java` 可匹配 `java.exe` / `JAVA.EXE`）；
/// 未监听任何端口的同名进程不出现在结果中；未找到时返回空列表。
pub fn process_ports(image: &str) -> Vec<ProcessPorts> {
    let entries = listening_tcp_entries();
    if entries.is_empty() {
        return Vec::new();
    }
    let mut pids: Vec<u32> = entries.iter().map(|e| e.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    let names = pid_image_names(&pids);

    // 按 PID 分组收集端口，只保留镜像名匹配的进程
    let mut by_pid: BTreeMap<u32, Vec<u16>> = BTreeMap::new();
    for e in &entries {
        if names
            .get(&e.pid)
            .is_some_and(|name| image_matches(name, image))
        {
            by_pid.entry(e.pid).or_default().push(e.port);
        }
    }
    by_pid
        .into_iter()
        .map(|(pid, mut ports)| {
            ports.sort_unstable();
            ports.dedup();
            ProcessPorts {
                name: names.get(&pid).cloned().unwrap_or_default(),
                pid,
                ports,
            }
        })
        .collect()
}

/// 镜像名匹配：忽略首尾空白、大小写与 .exe 后缀
/// （输入 `java` 可匹配进程 `java.exe` / `JAVA.EXE`）
fn image_matches(image: &str, wanted: &str) -> bool {
    // 先统一小写再剥后缀：strip_suffix 区分大小写，JAVA.EXE 需先归一才能剥掉
    let normalize = |s: &str| {
        let lowered = s.trim().to_ascii_lowercase();
        lowered.strip_suffix(".exe").unwrap_or(&lowered).to_string()
    };
    normalize(image) == normalize(wanted)
}

/// 批量读取进程镜像名：返回 PID → 镜像名（进程已退出或读取不到的不在表中）
fn pid_image_names(pids: &[u32]) -> HashMap<u32, String> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let pid_list: Vec<Pid> = pids.iter().map(|&p| Pid::from_u32(p)).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pid_list),
        true,
        ProcessRefreshKind::everything(),
    );
    pid_list
        .iter()
        .filter_map(|pid| {
            sys.process(*pid)
                .map(|pr| (pid.as_u32(), pr.name().to_string_lossy().to_string()))
        })
        .collect()
}

/// 表项本地端口：低 16 位为网络字节序的端口号，转换为本机序
#[cfg(windows)]
fn ntohs_port(dw_port: u32) -> u16 {
    ((dw_port & 0xffff) as u16).swap_bytes()
}

/// 枚举 IPv4 全部监听表项（过滤 PID ≤ 4 的内核/System 项）
#[cfg(windows)]
fn listening_entries_v4() -> Vec<ListenEntry> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    const MIB_TCP_STATE_LISTEN: u32 = 2;
    let class = TCP_TABLE_OWNER_PID_LISTENER;
    unsafe {
        // 第一次调用传空缓冲区，取所需大小（返回 ERROR_INSUFFICIENT_BUFFER 并填充 size）
        let mut size: u32 = 0;
        let _ = GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, class, 0);
        if size == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedTcpTable(buf.as_mut_ptr() as *mut _, &mut size, 0, AF_INET as u32, class, 0) != 0
        {
            return Vec::new();
        }
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let rows =
            std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        rows.iter()
            .filter(|r| r.dwState == MIB_TCP_STATE_LISTEN && r.dwOwningPid > 4)
            .map(|r| ListenEntry {
                port: ntohs_port(r.dwLocalPort),
                pid: r.dwOwningPid,
            })
            .collect()
    }
}

/// 枚举 IPv6 全部监听表项（Java 等服务常绑定 `::` 双栈地址）
#[cfg(windows)]
fn listening_entries_v6() -> Vec<ListenEntry> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET6;

    const MIB_TCP_STATE_LISTEN: u32 = 2;
    let class = TCP_TABLE_OWNER_PID_LISTENER;
    unsafe {
        let mut size: u32 = 0;
        let _ = GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET6 as u32, class, 0);
        if size == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size as usize];
        if GetExtendedTcpTable(buf.as_mut_ptr() as *mut _, &mut size, 0, AF_INET6 as u32, class, 0)
            != 0
        {
            return Vec::new();
        }
        let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
        let rows =
            std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        rows.iter()
            .filter(|r| r.dwState == MIB_TCP_STATE_LISTEN && r.dwOwningPid > 4)
            .map(|r| ListenEntry {
                port: ntohs_port(r.dwLocalPort),
                pid: r.dwOwningPid,
            })
            .collect()
    }
}

/// 暂停自动拉起的标记文件路径：与配置文件同目录同名，扩展名改为 pause
/// （如 `watchdog.toml` → `watchdog.pause`）。
///
/// 文件由 GUI 的「全部停止」写入、看门狗监控循环每轮检查：存在时暂停自动拉起
/// 与假死恢复（已拉起的程序保持运行），删除后恢复。避免「全部停止」被服务
/// 在下个检测周期重新拉起。
pub fn pause_flag_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("pause")
}

/// 探测 host:port 的 TCP 端口是否可连通（用于端口方式判定程序是否在运行）。
///
/// 主机名解析出的多个地址（如 localhost 的 IPv4/IPv6）任一可连通即视为通；
/// 解析失败、连接被拒或超时均视为不通。
pub fn port_listening(host: &str, port: u16) -> bool {
    match format!("{host}:{port}").to_socket_addrs() {
        Ok(mut addrs) => addrs
            .any(|a| TcpStream::connect_timeout(&a, PORT_CHECK_TIMEOUT).is_ok()),
        Err(_) => false,
    }
}

/// 命令行解析结果
pub struct ParsedArgs {
    /// 子命令（run / foreground / validate / version / port / proc，缺省 run）
    pub command: String,
    /// 子命令的位置参数：port 的端口号、proc 的进程名；其它子命令为 None
    pub arg: Option<String>,
    /// `--config` / `-c` 指定的配置文件路径
    pub config: Option<PathBuf>,
}

/// 解析命令行参数（不含程序名，CLI 侧传 `args[1..]`；服务侧传 SCM 的启动参数）。
///
/// 子命令取最后出现的 run / foreground / validate / version / port / proc；
/// port / proc 的位置参数取第一个无法识别的非「-」开头令牌；
/// `--config <path>` 指定配置文件。
pub fn parse_args(args: &[String]) -> ParsedArgs {
    let mut parsed = ParsedArgs {
        command: String::from("run"),
        arg: None,
        config: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                if i + 1 < args.len() {
                    parsed.config = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
            }
            "run" | "foreground" | "validate" | "version" | "port" | "proc" => {
                parsed.command = args[i].clone()
            }
            "--version" | "-V" => parsed.command = String::from("version"),
            _ => {
                if parsed.arg.is_none() && !args[i].starts_with('-') {
                    parsed.arg = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_listening_detects_listener() {
        // 绑定临时端口 → 可连通
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定失败");
        let port = listener.local_addr().expect("取地址失败").port();
        assert!(port_listening("127.0.0.1", port), "监听中的端口应视为可连通");

        // 关闭监听 → 不可连通（临时端口随即释放，连接被拒）
        drop(listener);
        assert!(
            !port_listening("127.0.0.1", port),
            "已关闭的端口应视为不可连通"
        );

        // 无法解析的主机名 → 不可连通
        assert!(!port_listening("no.such.host.invalid", 80));
    }

    #[test]
    fn pause_flag_path_uses_config_stem() {
        assert_eq!(
            pause_flag_path(Path::new(r"C:\Watchdog\watchdog.toml")),
            PathBuf::from(r"C:\Watchdog\watchdog.pause")
        );
        // 无扩展名的配置文件也能生成同目录标记
        assert_eq!(
            pause_flag_path(Path::new(r"C:\Watchdog\mycfg")),
            PathBuf::from(r"C:\Watchdog\mycfg.pause")
        );
    }

    /// 按监听端口查 PID：本测试进程绑定临时端口后应查到自身，关闭后查不到
    #[cfg(target_os = "windows")]
    #[test]
    fn pids_listening_port_finds_owner() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定失败");
        let port = listener.local_addr().expect("取地址失败").port();
        assert!(
            pids_listening_port(port).contains(&std::process::id()),
            "应查到监听该端口的自身进程"
        );
        drop(listener);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            pids_listening_port(port).is_empty(),
            "端口释放后不应再查到监听进程"
        );
    }

    /// 镜像名匹配：忽略大小写与 .exe 后缀
    #[test]
    fn image_matches_ignores_case_and_exe_suffix() {
        assert!(image_matches("java.exe", "java"));
        assert!(image_matches("JAVA.EXE", "java"));
        assert!(image_matches("java.exe", "JAVA.EXE"));
        assert!(image_matches("java", "java"));
        assert!(!image_matches("javaw.exe", "java"), "前缀相同但后缀不同不应匹配");
        assert!(!image_matches("java.exe", "javaw"));
    }

    /// 按端口查进程名/PID、按进程名查端口：双向查询应能查到绑定临时端口的自身进程
    #[cfg(target_os = "windows")]
    #[test]
    fn port_process_queries_roundtrip() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定失败");
        let port = listener.local_addr().expect("取地址失败").port();

        // 按端口查：应查到自身 PID，且进程名非空
        let owners = port_owners(port);
        let own = owners
            .iter()
            .find(|o| o.pid == std::process::id())
            .expect("按端口应查到自身进程");
        assert!(!own.name.is_empty(), "查到的进程名不应为空");

        // 按进程名查：应反查到该端口
        let groups = process_ports(&own.name);
        let group = groups
            .iter()
            .find(|g| g.pid == std::process::id())
            .expect("按进程名应查到自身进程");
        assert!(
            group.ports.contains(&port),
            "按进程名应查到监听端口 {port}，实际 {:?}",
            group.ports
        );

        // 省略 .exe 后缀的进程名也应能匹配
        let short = own.name.strip_suffix(".exe").unwrap_or(&own.name).to_string();
        assert!(
            process_ports(&short).iter().any(|g| g.ports.contains(&port)),
            "省略 .exe 的进程名「{short}」也应查到端口 {port}"
        );

        // 端口释放后按端口应查不到进程
        drop(listener);
        std::thread::sleep(Duration::from_millis(200));
        assert!(port_owners(port).is_empty(), "端口释放后不应查到监听进程");
    }

    /// parse_args：port / proc 子命令的位置参数与 --config 共存时的解析
    #[test]
    fn parse_args_captures_positional_arg() {
        let to_args = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };

        // port 子命令 + 端口号 + 配置路径
        let parsed = parse_args(&to_args(&["port", "8080", "--config", "d:\\a.toml"]));
        assert_eq!(parsed.command, "port");
        assert_eq!(parsed.arg.as_deref(), Some("8080"));
        assert_eq!(parsed.config, Some(PathBuf::from("d:\\a.toml")));

        // proc 子命令 + 进程名
        let parsed = parse_args(&to_args(&["proc", "java.exe"]));
        assert_eq!(parsed.command, "proc");
        assert_eq!(parsed.arg.as_deref(), Some("java.exe"));

        // 无位置参数 / 缺省子命令不受影响
        let parsed = parse_args(&to_args(&["--config", "a.toml"]));
        assert_eq!(parsed.command, "run");
        assert_eq!(parsed.arg, None);
        assert_eq!(parsed.config, Some(PathBuf::from("a.toml")));

        // 「-」开头的未知参数不作为位置参数
        let parsed = parse_args(&to_args(&["port", "-x"]));
        assert_eq!(parsed.command, "port");
        assert_eq!(parsed.arg, None);
    }
}
