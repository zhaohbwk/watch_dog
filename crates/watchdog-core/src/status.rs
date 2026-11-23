//! 进程运行状态扫描与单个程序的手动启停控制。
//!
//! 检测规则与监控循环（monitor.rs）完全一致：配置了检测端口时以端口可连通为准，
//! 否则按进程镜像名匹配或命令行包含匹配（jar/bat/ps1）。
//! 停止配置了检测端口的程序时，优先按监听该端口的 PID 精确终止（不误杀同名进程），
//! 端口上无监听进程时才回退按检测规则匹配。
//! 供 GUI 显示红/绿状态灯及「启动/停止/重启」单个程序、批量全部启动/停止使用。

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::config::EffectiveProgram;
use crate::launcher;
use crate::util;

/// 系统进程快照：（PID, 镜像名小写, 命令行小写）
pub struct Snapshot {
    procs: Vec<(Pid, String, String)>,
}

impl Snapshot {
    /// 扫描当前系统进程（含命令行信息）
    pub fn scan() -> Self {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );
        let procs = sys
            .processes()
            .values()
            .map(|p| {
                let image = p.name().to_string_lossy().to_ascii_lowercase();
                let cmdline = p
                    .cmd()
                    .iter()
                    .filter_map(|a| a.to_str())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase();
                (p.pid(), image, cmdline)
            })
            .collect();
        Snapshot { procs }
    }

    /// 返回与程序检测规则匹配的所有进程 PID
    pub fn matching_pids(&self, prog: &EffectiveProgram) -> Vec<Pid> {
        self.procs
            .iter()
            .filter(|(_, image, cmdline)| {
                image.eq_ignore_ascii_case(&prog.detect_image)
                    || prog
                        .detect_cmdline
                        .as_ref()
                        .is_some_and(|needle| !needle.is_empty() && cmdline.contains(needle))
            })
            .map(|(pid, _, _)| *pid)
            .collect()
    }

    /// 判断程序当前是否在运行：
    /// 配置了检测端口 → 以端口可连通为准；否则按进程规则匹配。
    /// （快照仅在无端口检测时使用，端口检测直接发起 TCP 连接）
    pub fn is_running(&self, prog: &EffectiveProgram) -> bool {
        match prog.detect_addr() {
            Some((host, port)) => util::port_listening(host, port),
            None => !self.matching_pids(prog).is_empty(),
        }
    }
}

/// 一次性扫描并返回各程序的运行状态（与入参顺序一致）。
pub fn scan_running(programs: &[EffectiveProgram]) -> Vec<bool> {
    let snapshot = Snapshot::scan();
    programs.iter().map(|p| snapshot.is_running(p)).collect()
}

/// 手动启动单个程序（不经过检测，立即拉起）。
///
/// `out_log` 语义与 `launcher::launch` 一致：Some 时把程序输出记录到该文件。
pub fn start_program(prog: &EffectiveProgram, out_log: Option<&Path>) -> Result<(), String> {
    if !prog.path.exists() {
        return Err(format!("程序路径不存在：{}", prog.path.display()));
    }
    launcher::launch(prog, out_log)
        .map(|_| ())
        .map_err(|e| format!("启动失败：{e}"))
}

/// 终止指定 PID 列表的进程，返回成功终止的数量。
///
/// 进程已不存在的 PID 不计入失败；全部因权限等原因终止失败时报错。
fn kill_pids(pids: &[u32]) -> Result<usize, String> {
    if pids.is_empty() {
        return Ok(0);
    }
    let pid_list: Vec<Pid> = pids.iter().map(|&p| Pid::from_u32(p)).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pid_list),
        true,
        ProcessRefreshKind::everything(),
    );
    let mut killed = 0usize;
    let mut missed = 0usize;
    for pid in &pid_list {
        match sys.process(*pid) {
            Some(process) if process.kill() => killed += 1,
            Some(_) => missed += 1,
            None => {} // 进程已自行退出，不计入
        }
    }
    if killed == 0 && missed > 0 {
        return Err(format!(
            "找到 {missed} 个目标进程但终止失败（可能权限不足，请以管理员身份运行）"
        ));
    }
    Ok(killed)
}

/// 手动停止单个程序：终止程序对应的进程，返回成功终止的进程数。
///
/// 配置了检测端口的程序**优先按监听端口的 PID 精确终止**——只杀真正监听该端口的
/// 进程，不误杀镜像名/命令行相同的其它程序；端口上无监听进程时（程序未运行、
/// 假死进程已不监听、或从未监听所配置端口）回退按检测规则匹配。
/// 注意：未配置端口的 exe 按镜像名匹配时，同名的其它实例也会被终止。
pub fn stop_program(prog: &EffectiveProgram) -> Result<usize, String> {
    if let Some(port) = prog.detect_port {
        let pids = util::pids_listening_port(port);
        if !pids.is_empty() {
            return kill_pids(&pids);
        }
    }

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );
    let mut killed = 0usize;
    let mut missed = 0usize;
    for process in sys.processes().values() {
        let image = process.name().to_string_lossy().to_ascii_lowercase();
        let cmdline = process
            .cmd()
            .iter()
            .filter_map(|a| a.to_str())
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        let matched = image.eq_ignore_ascii_case(&prog.detect_image)
            || prog
                .detect_cmdline
                .as_ref()
                .is_some_and(|needle| !needle.is_empty() && cmdline.contains(needle));
        if matched {
            if process.kill() {
                killed += 1;
            } else {
                missed += 1;
            }
        }
    }
    if killed == 0 && missed > 0 {
        return Err(format!(
            "找到 {missed} 个匹配进程但终止失败（可能权限不足，请以管理员身份运行）"
        ));
    }
    Ok(killed)
}

/// 手动重启单个程序：先停止（等待退出），再启动。
pub fn restart_program(prog: &EffectiveProgram, out_log: Option<&Path>) -> Result<String, String> {
    let killed = stop_program(prog)?;
    // 等待匹配进程完全退出（最多 10 秒）
    if killed > 0 {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if Snapshot::scan().matching_pids(prog).is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    start_program(prog, out_log)?;
    Ok(format!("已终止 {killed} 个进程并重新启动"))
}

/// 批量全部启动 / 全部停止中单个程序的结果（供 GUI 汇总提示）
pub struct BatchResult {
    /// 程序名称
    pub name: String,
    /// 已跳过：skip = true；启动时已在运行；停止时未在运行
    pub skipped: bool,
    /// 启动：拉起的程序数（0 或 1）；停止：终止的进程数
    pub count: usize,
    /// 失败原因；None = 成功或跳过
    pub error: Option<String>,
}

/// 一键启动所有程序：只启动未跳过且未运行的，避免拉起重复实例。
///
/// 与单程序手动启动一致，程序输出记录到与看门狗日志同目录的 `<名称>.log`。
pub fn start_all_programs(
    programs: &[EffectiveProgram],
    watchdog_log: &Path,
) -> Vec<BatchResult> {
    let snapshot = Snapshot::scan();
    programs
        .iter()
        .map(|prog| {
            let mut result = BatchResult {
                name: prog.name.clone(),
                skipped: false,
                count: 0,
                error: None,
            };
            if prog.skip {
                result.skipped = true;
                return result;
            }
            if snapshot.is_running(prog) {
                result.skipped = true;
                return result;
            }
            if !prog.path.exists() {
                result.error = Some(format!("程序路径不存在：{}", prog.path.display()));
                return result;
            }
            let out_log = util::program_log_path(watchdog_log, &prog.name);
            match launcher::launch(prog, Some(&out_log)) {
                Ok(_) => result.count = 1,
                Err(e) => result.error = Some(format!("启动失败：{e}")),
            }
            result
        })
        .collect()
}

/// 一键停止所有程序：按配置逆序终止匹配进程（后启动的先停，符合依赖关闭顺序）。
///
/// 复用 [`stop_program`] 的检测规则与进程匹配口径。
pub fn stop_all_programs(programs: &[EffectiveProgram]) -> Vec<BatchResult> {
    programs
        .iter()
        .rev()
        .map(|prog| {
            let mut result = BatchResult {
                name: prog.name.clone(),
                skipped: false,
                count: 0,
                error: None,
            };
            if prog.skip {
                result.skipped = true;
                return result;
            }
            match stop_program(prog) {
                Ok(n) => result.count = n,
                Err(e) => result.error = Some(e),
            }
            result
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Program;
    use std::path::PathBuf;

    // 用 sleep 模拟常驻进程，验证 手动启动→检测运行→停止→检测退出 的完整链路
    #[test]
    fn start_stop_restart_roundtrip() {
        // 注意：不能用 timeout.exe（stdin 被重定向时会立即报错退出），
        // ping -n 是 Windows 上经典的“无控制台也可常驻”的睡眠方式
        #[cfg(target_os = "windows")]
        let (path, args) = (
            PathBuf::from("C:\\Windows\\System32\\ping.exe"),
            vec!["-n".into(), "60".into(), "127.0.0.1".into()],
        );
        #[cfg(not(target_os = "windows"))]
        let (path, args) = (PathBuf::from("/bin/sleep"), vec!["60".into()]);

        let prog = Program {
            name: "ctl-test".into(),
            path,
            args,
            workdir: None,
            java: None,
            detect: None,
            max_restarts_per_hour: 0,
            start_delay_secs: 0,
            skip: false,
            log_file: None,
        }
        .to_effective();

        // 输出日志：临时文件，测试结束时清理
        let log_path = std::env::temp_dir().join("watchdog-ctl-test-out.log");
        let _ = std::fs::remove_file(&log_path);

        // 分隔行按字节匹配：子进程输出可能是任意编码（如中文控制台的 GBK），
        // 文件不能按 UTF-8 文本整体读取
        let needle = "看门狗启动".as_bytes();
        let count_needle = |bytes: &[u8]| -> usize {
            bytes.windows(needle.len()).filter(|w| *w == needle).count()
        };

        // 1) 手动启动 → 应处于运行状态，且输出日志中写入启动分隔行
        start_program(&prog, Some(&log_path)).expect("启动失败");
        assert!(log_path.exists(), "启动后应生成输出日志文件");
        let log_bytes = std::fs::read(&log_path).expect("读取输出日志失败");
        assert!(
            count_needle(&log_bytes) >= 1,
            "输出日志应含启动分隔行，实际：{:?}",
            String::from_utf8_lossy(&log_bytes)
        );
        thread::sleep(Duration::from_millis(500));
        let snapshot = Snapshot::scan();
        assert!(snapshot.is_running(&prog), "启动后应检测到运行");

        // 2) 停止 → 应退出
        let killed = stop_program(&prog).expect("停止失败");
        assert!(killed >= 1, "至少终止 1 个进程，实际 {killed}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if Snapshot::scan().matching_pids(&prog).is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(
            Snapshot::scan().matching_pids(&prog).is_empty(),
            "停止后不应再有匹配进程"
        );

        // 3) 重启 → 应重新运行，分隔行追加第二条
        restart_program(&prog, Some(&log_path)).expect("重启失败");
        thread::sleep(Duration::from_millis(500));
        assert!(Snapshot::scan().is_running(&prog), "重启后应检测到运行");
        let log_bytes = std::fs::read(&log_path).expect("读取输出日志失败");
        assert_eq!(
            count_needle(&log_bytes),
            2,
            "重启后应累计两条启动分隔行，实际：{:?}",
            String::from_utf8_lossy(&log_bytes)
        );

        // 4) 清理：再次停止
        stop_program(&prog).expect("清理失败");
        let _ = std::fs::remove_file(&log_path);
    }

    /// 一键全部启动 / 全部停止：启动未运行的、跳过 skip = true 的、逆序终止进程
    #[test]
    fn start_all_stop_all_batch() {
        // 复制为临时目录下的唯一文件名再运行：检测按镜像名（= 文件名）匹配，
        // 避免与并行运行的其它测试拉起的 ping.exe / sleep 进程互相误判
        #[cfg(target_os = "windows")]
        let exe_src = PathBuf::from("C:\\Windows\\System32\\ping.exe");
        #[cfg(not(target_os = "windows"))]
        let exe_src = PathBuf::from("/bin/sleep");
        let path = std::env::temp_dir().join("watchdog-batch-prog.exe");
        std::fs::copy(&exe_src, &path).expect("复制测试程序失败");
        let args: Vec<String> = if cfg!(target_os = "windows") {
            vec!["-n".into(), "60".into(), "127.0.0.1".into()]
        } else {
            vec!["60".into()]
        };

        // skip = true 的条目不应被批量操作触碰（用不存在的路径验证：若被启动会报错）
        let skipped = Program {
            name: "batch-skipped".into(),
            path: PathBuf::from(r"C:\nonexistent\missing.exe"),
            args: vec![],
            workdir: None,
            java: None,
            detect: None,
            max_restarts_per_hour: 0,
            start_delay_secs: 0,
            skip: true,
            log_file: None,
        }
        .to_effective();
        let prog = Program {
            name: "batch-main".into(),
            path: path.clone(),
            args,
            workdir: None,
            java: None,
            detect: None,
            max_restarts_per_hour: 0,
            start_delay_secs: 0,
            skip: false,
            log_file: None,
        }
        .to_effective();
        let programs = vec![prog.clone(), skipped.clone()];

        // 输出日志基准：临时文件，测试结束时清理
        let base_log = std::env::temp_dir().join("watchdog-batch-test.log");
        let out_log = crate::util::program_log_path(&base_log, "batch-main");
        let _ = std::fs::remove_file(&out_log);

        // 1) 全部启动：正常运行 + skip 条目跳过
        let results = start_all_programs(&programs, &base_log);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "batch-main");
        assert!(!results[0].skipped, "正常运行条目不应标记跳过");
        assert_eq!(results[0].count, 1, "应拉起 1 个程序");
        assert!(results[0].error.is_none(), "启动不应失败：{:?}", results[0].error);
        assert!(results[1].skipped, "skip = true 的条目应被跳过");
        assert!(out_log.exists(), "批量启动应写入程序输出日志");
        thread::sleep(Duration::from_millis(500));
        assert!(
            Snapshot::scan().is_running(&prog),
            "全部启动后应检测到运行"
        );

        // 2) 再次全部启动：已在运行 → 跳过，不拉起重复实例
        let results = start_all_programs(&programs, &base_log);
        assert!(results[0].skipped, "已在运行的条目应跳过");
        assert_eq!(results[0].count, 0);

        // 3) 全部停止：skip 条目跳过，正常运行条目按检测规则终止
        //    （逆序返回：后配置的先停，因此 skip 条目排在前面）
        let results = stop_all_programs(&programs);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "batch-skipped");
        assert!(results[0].skipped, "skip = true 的条目应被跳过");
        assert_eq!(results[1].name, "batch-main");
        assert!(results[1].count >= 1, "至少终止 1 个进程，实际 {}", results[1].count);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if Snapshot::scan().matching_pids(&prog).is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(
            Snapshot::scan().matching_pids(&prog).is_empty(),
            "全部停止后不应再有匹配进程"
        );
        let _ = std::fs::remove_file(&out_log);
        let _ = std::fs::remove_file(&path);
    }

    /// 端口检测程序的停止：按监听端口的 PID 精确终止（镜像名规则故意配错也能停掉）
    #[cfg(target_os = "windows")]
    #[test]
    fn stop_program_by_port_kills_listener() {
        use crate::config::DetectConfig;
        use std::io::BufRead;

        // 子进程绑定临时端口并打印端口号后驻留（父进程读到端口后按端口停止它）
        let script = "$l=[System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,0);$l.Start();Write-Output $l.LocalEndpoint.Port;Start-Sleep -Seconds 30";
        let mut child = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", script])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("启动监听子进程失败");
        let port: u16 = {
            let stdout = child.stdout.take().expect("取子进程输出失败");
            let mut line = String::new();
            std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .expect("读取子进程端口失败");
            line.trim().parse().expect("端口号解析失败")
        };

        // 检测规则刻意配错（镜像名匹配不到任何进程）：停止必须走端口路径才能找到进程
        let prog = Program {
            name: "port-stop".into(),
            path: PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            args: vec![],
            workdir: None,
            java: None,
            detect: Some(DetectConfig {
                image: Some("no-such-image.exe".into()),
                cmdline_contains: None,
                port: Some(port),
                host: None,
            }),
            max_restarts_per_hour: 0,
            start_delay_secs: 0,
            skip: false,
            log_file: None,
        }
        .to_effective();

        // 等子进程完成监听（PowerShell 启动需 1~2 秒）
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !util::port_listening("127.0.0.1", port) {
            thread::sleep(Duration::from_millis(200));
        }
        assert!(
            Snapshot::scan().is_running(&prog),
            "停止前应通过端口检测到程序运行中"
        );

        // 按端口精确停止：应找到监听 PID 并终止（规则匹配不到任何进程也不影响）
        let killed = stop_program(&prog).expect("按端口停止失败");
        assert!(killed >= 1, "应按端口终止至少 1 个进程，实际 {killed}");

        // 进程终止后端口不再可连通
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && util::port_listening("127.0.0.1", port) {
            thread::sleep(Duration::from_millis(200));
        }
        assert!(
            !util::port_listening("127.0.0.1", port),
            "停止后端口不应再可连通"
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
