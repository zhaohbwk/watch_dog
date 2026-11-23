//! 按文件类型拉起被监控程序。

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::config::EffectiveProgram;
use crate::logger::{now_string, MAX_LOG_SIZE};

/// CREATE_NO_WINDOW：隐藏 bat/cmd/powershell 的控制台窗口
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 启动程序，返回子进程句柄（由监控循环跟踪其退出）。
///
/// `out_log` 为 Some 时，子进程的 stdout/stderr 追加重定向到该文件，
/// 并在拉起前写入一行启动分隔标记，便于在 GUI 中按程序查看输出、区分多次运行；
/// 日志文件无法创建时退回原来的行为（不重定向，不影响拉起本身）。
pub fn launch(prog: &EffectiveProgram, out_log: Option<&Path>) -> io::Result<Child> {
    let ext = prog.extension();
    let mut cmd = match ext.as_str() {
        "exe" => {
            let mut c = Command::new(&prog.path);
            c.args(&prog.args);
            c
        }
        "bat" | "cmd" => {
            let mut c = Command::new("cmd.exe");
            c.arg("/c").arg(&prog.path).args(&prog.args);
            hide_window(&mut c);
            c
        }
        "jar" => {
            let java = prog.java.clone().unwrap_or_else(resolve_java);
            let mut c = Command::new(&java);
            c.arg("-jar").arg(&prog.path).args(&prog.args);
            hide_window(&mut c);
            c
        }
        "ps1" => {
            let mut c = Command::new("powershell.exe");
            c.arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(&prog.path)
                .args(&prog.args);
            hide_window(&mut c);
            c
        }
        // 未知扩展名：尝试直接执行
        _ => {
            let mut c = Command::new(&prog.path);
            c.args(&prog.args);
            c
        }
    };

    // 工作目录：未配置时默认为程序所在目录（exe/bat/jar 常依赖相对路径找资源，
    // 以看门狗的工作目录启动会导致启动失败）
    let workdir = prog
        .workdir
        .clone()
        .or_else(|| prog.path.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    if !workdir.as_os_str().is_empty() {
        cmd.current_dir(workdir);
    }

    if let Some(log) = out_log {
        match open_program_log(log, prog) {
            Ok(file) => {
                // stdout/stderr 指向同一句柄（try_clone 复制描述符），追加写入同一文件
                match file.try_clone() {
                    Ok(stderr) => {
                        cmd.stdout(file);
                        cmd.stderr(stderr);
                    }
                    Err(_) => {
                        cmd.stdout(file);
                    }
                }
            }
            // 输出日志打不开（目录不可写等）不应阻止程序拉起
            Err(_) => {}
        }
    }
    cmd.spawn()
}

/// 打开程序输出日志（追加模式）：先尽力轮转，再写入本次启动分隔行。
fn open_program_log(log: &Path, prog: &EffectiveProgram) -> io::Result<fs::File> {
    rotate_if_needed(log);
    if let Some(dir) = log.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
    let args = if prog.args.is_empty() {
        String::new()
    } else {
        format!(" {}", prog.args.join(" "))
    };
    writeln!(
        file,
        "===== [{}] 看门狗启动：{}{} =====",
        now_string(),
        prog.path.display(),
        args
    )?;
    Ok(file)
}

/// 超过大小阈值时改名 `.log.1`（尽力而为：旧子进程仍持有句柄时改名失败则继续追加）。
fn rotate_if_needed(path: &Path) {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > MAX_LOG_SIZE {
            let backup = path.with_extension("log.1");
            let _ = fs::remove_file(&backup);
            let _ = fs::rename(path, &backup);
        }
    }
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

/// 解析 java.exe 路径：优先 JAVA_HOME\bin\java.exe，否则依赖 PATH。
pub fn resolve_java() -> PathBuf {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let candidate = Path::new(&home).join("bin").join("java.exe");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("java.exe")
}
