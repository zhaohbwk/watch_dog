//! 监控主循环：定期检测配置的程序是否运行，未运行则自动拉起。
//!
//! 检测方式：配置了检测端口（`detect.port`）的程序以端口可连通为准，端口持续不通
//! 且进程仍在时判定假死，先结束匹配进程再重新拉起；其余程序按进程规则匹配
//! （镜像名 / 命令行），与 GUI 状态灯共用 status::Snapshot 的判定。
//!
//! 暂停自动拉起：配置文件同目录的 `*.pause` 标记（GUI「全部停止」写入）存在时，
//! 本循环不做检测与拉起，已拉起的程序保持运行；标记移除后自动恢复。

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::launcher;
use crate::logger::Logger;
use crate::status::{self, Snapshot};
use crate::util;
use crate::EffectiveProgram;

/// 重启节流的统计窗口
const THROTTLE_WINDOW: Duration = Duration::from_secs(3600);

/// 端口检测的假死判定宽限期：端口持续不通且进程仍在超过该时长，才结束进程并重新拉起。
/// 覆盖服务启动慢的情况（如 Java 服务开机需几十秒才打开端口）。
const PORT_FAIL_GRACE: Duration = Duration::from_secs(60);

/// 每个被监控程序的运行时状态
struct ProgramState {
    /// 看门狗自己拉起的子进程句柄（None 表示未由本实例管理）
    child: Option<Child>,
    tracker: RestartTracker,
    /// 是否已过了启动延迟期（做过首次拉起或延迟已到期）
    first_launch_done: bool,
    /// 是否已记录过“延迟中暂不拉起”的日志（避免每轮刷屏）
    defer_logged: bool,
    /// 端口检测：端口持续不通的起始时刻（None = 端口正常或刚判定过假死）
    port_down_since: Option<Instant>,
}

/// 一小时内启动次数统计，用于防止崩溃循环
struct RestartTracker {
    times: VecDeque<Instant>,
}

impl RestartTracker {
    fn new() -> Self {
        RestartTracker {
            times: VecDeque::new(),
        }
    }

    /// 丢弃窗口期之外的记录
    fn prune(&mut self) {
        let cutoff = Instant::now() - THROTTLE_WINDOW;
        while self.times.front().is_some_and(|t| *t < cutoff) {
            self.times.pop_front();
        }
    }

    /// 是否还允许再次启动；`max_per_hour == 0` 表示不限制
    fn allowed(&mut self, max_per_hour: u32) -> bool {
        self.prune();
        max_per_hour == 0 || (self.times.len() as u32) < max_per_hour
    }

    fn record(&mut self) {
        self.times.push_back(Instant::now());
    }
}

/// 运行监控循环，直到 `stop` 被置位。
///
/// `config_path` 用于定位暂停标记（`pause_flag_path`）：标记存在时暂停自动拉起
/// 与假死恢复（已拉起的程序保持运行），供 GUI「全部停止」使用。
pub fn run_loop(
    config: &Config,
    config_path: &Path,
    logger: &Logger,
    stop: &AtomicBool,
) -> Result<(), String> {
    let pause_flag = util::pause_flag_path(config_path);
    let all: Vec<EffectiveProgram> = config.effective_programs();
    // skip = true 的程序：本轮看门狗不监控、不自动拉起（GUI 手动启停不受影响）
    let (programs, skipped) = split_skipped(all);
    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|p| p.name.as_str()).collect();
        logger.info(&format!("跳过已停用的程序：{}", names.join("，")));
    }
    if programs.is_empty() && !skipped.is_empty() {
        logger.warn("所有程序均已跳过，看门狗将不监控任何程序");
    }
    let interval = Duration::from_secs(config.check_interval_secs.max(1));
    // 启动延迟的计时基准：看门狗本轮运行开始时刻
    let started_at = Instant::now();

    let mut states: HashMap<String, ProgramState> = programs
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                ProgramState {
                    child: None,
                    tracker: RestartTracker::new(),
                    first_launch_done: p.start_delay_secs == 0,
                    defer_logged: false,
                    port_down_since: None,
                },
            )
        })
        .collect();

    let delayed: Vec<&EffectiveProgram> = programs.iter().filter(|p| p.start_delay_secs > 0).collect();
    if !delayed.is_empty() {
        logger.info(&format!(
            "启动顺序控制：{}",
            delayed
                .iter()
                .map(|p| format!("{}（{} 秒后）", p.name, p.start_delay_secs))
                .collect::<Vec<_>>()
                .join("，")
        ));
    }

    let port_based: Vec<&EffectiveProgram> = programs
        .iter()
        .filter(|p| p.detect_addr().is_some())
        .collect();
    if !port_based.is_empty() {
        logger.info(&format!(
            "端口检测（以此为准）：{}",
            port_based
                .iter()
                .filter_map(|p| {
                    p.detect_addr()
                        .map(|(host, port)| format!("{}（{host}:{port}）", p.name))
                })
                .collect::<Vec<_>>()
                .join("，")
        ));
    }

    logger.info(&format!(
        "看门狗启动：监控 {} 个程序，检测间隔 {} 秒",
        programs.len(),
        interval.as_secs()
    ));

    // 程序输出日志的基准路径：各程序日志与看门狗日志同目录（<name>.log）
    let watchdog_log = util::resolve_log_path(config);

    // 上一轮的暂停状态：仅在进入/退出暂停时各记一条日志，避免每轮刷屏
    let mut last_paused = false;

    loop {
        if stop.load(Ordering::SeqCst) {
            logger.info("收到停止信号，看门狗退出（已拉起的子进程保持运行）");
            break;
        }

        // 暂停标记（GUI「全部停止」写入）：存在时本轮不检测、不拉起，
        // 已拉起的程序保持运行；标记移除后自动恢复
        let paused = pause_flag.exists();
        if paused != last_paused {
            last_paused = paused;
            if paused {
                logger.info(&format!(
                    "检测到暂停标记 {}，暂停自动拉起与假死恢复（已拉起的程序保持运行）",
                    pause_flag.display()
                ));
            } else {
                logger.info("暂停标记已移除，恢复自动拉起");
            }
        }

        if !paused {
            // 每轮扫描一次系统进程（含命令行信息，用于 jar/bat 的匹配）；
            // 检测规则统一走 status::Snapshot，与 GUI 状态灯的判定保持一致
            let snapshot = Snapshot::scan();

            for prog in &programs {
                let state = states
                    .get_mut(&prog.name)
                    .ok_or_else(|| format!("内部错误：缺少状态 {}", prog.name))?;
                check_program(prog, &snapshot, state, started_at, logger, &watchdog_log);
            }
        }

        // 分片睡眠，及时响应停止信号
        let deadline = Instant::now() + interval;
        while Instant::now() < deadline {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    logger.info("看门狗退出");
    Ok(())
}

/// 按配置把程序列表分为（监控列表, 跳过列表）：`skip = true` 的进入跳过列表。
///
/// 注意方向：`partition` 把谓词为 true 的元素放进第一个向量，
/// 因此谓词必须取 `!p.skip`，监控列表才是未标记跳过的程序。
fn split_skipped(
    all: Vec<EffectiveProgram>,
) -> (Vec<EffectiveProgram>, Vec<EffectiveProgram>) {
    all.into_iter().partition(|p| !p.skip)
}

fn check_program(
    prog: &EffectiveProgram,
    snapshot: &Snapshot,
    state: &mut ProgramState,
    started_at: Instant,
    logger: &Logger,
    watchdog_log: &Path,
) {
    // 1) 看门狗拉起的子进程：退出时记录日志；
    //    未配置端口检测时，子进程仍存活直接视为运行中
    if let Some(child) = state.child.as_mut() {
        match child.try_wait() {
            Ok(None) => {
                if prog.detect_addr().is_none() {
                    state.tracker.prune();
                    return;
                }
            }
            Ok(Some(status)) => {
                state.child = None;
                logger.warn(&format!("[{}] 进程已退出（{status}）", prog.name));
            }
            Err(e) => {
                state.child = None;
                logger.error(&format!("[{}] 查询子进程状态失败：{e}", prog.name));
            }
        }
    }

    if let Some((host, port)) = prog.detect_addr() {
        // 2) 端口检测（以此为准）：端口可连通 → 运行中
        if util::port_listening(host, port) {
            state.first_launch_done = true;
            state.port_down_since = None;
            state.tracker.prune();
            return;
        }

        // 端口不通：宽限期内视为可能仍在启动，等待；超期且进程仍在 → 判定假死
        let down_since = *state.port_down_since.get_or_insert_with(Instant::now);
        if state.child.is_some() || !snapshot.matching_pids(prog).is_empty() {
            if down_since.elapsed() < PORT_FAIL_GRACE {
                return;
            }
            logger.warn(&format!(
                "[{}] 端口 {host}:{port} 持续不通且进程仍在，判定假死，结束匹配进程后重新拉起",
                prog.name
            ));
            // 先结束看门狗自己拉起的子进程（检测规则被改过时可能匹配不上它）
            if let Some(mut child) = state.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            match status::stop_program(prog) {
                Ok(n) if n > 0 => {
                    logger.info(&format!("[{}] 已终止 {n} 个匹配进程", prog.name));
                }
                Ok(_) => {}
                Err(e) => logger.error(&format!("[{}] 结束假死进程失败：{e}", prog.name)),
            }
            // 重新计算宽限期：等进程退出后由下个周期重新拉起
            state.port_down_since = None;
            return;
        }
        // 端口不通且无匹配进程：走下方常规拉起流程（启动延迟 / 重启节流）
    } else if !snapshot.matching_pids(prog).is_empty() {
        // 2) 进程检测：镜像名匹配 或 命令行包含匹配 → 运行中（含看门狗启动前就存在的情况）
        state.first_launch_done = true;
        state.tracker.prune();
        return;
    }

    // 3) 启动延迟未到 → 暂不拉起（只约束看门狗启动后的首次拉起）
    if !state.first_launch_done {
        let delay = Duration::from_secs(prog.start_delay_secs);
        let elapsed = started_at.elapsed();
        if elapsed < delay {
            if !state.defer_logged {
                logger.info(&format!(
                    "[{}] 启动延迟中，{} 秒后允许首次拉起",
                    prog.name,
                    (delay - elapsed).as_secs() + 1
                ));
                state.defer_logged = true;
            }
            return;
        }
        state.first_launch_done = true;
    }

    // 4) 未运行 → 尝试启动（带重启节流）
    if !state.tracker.allowed(prog.max_restarts_per_hour) {
        logger.warn(&format!(
            "[{}] 最近一小时内已自动启动 {} 次，达到上限，本周期跳过",
            prog.name, prog.max_restarts_per_hour
        ));
        return;
    }
    state.tracker.record();

    logger.warn(&format!(
        "[{}] 检测到未运行，尝试启动：{} {}",
        prog.name,
        prog.path.display(),
        prog.args.join(" ")
    ));

    if !prog.path.exists() {
        logger.error(&format!(
            "[{}] 程序路径不存在：{}",
            prog.name,
            prog.path.display()
        ));
        return;
    }

    let out_log = util::program_log_path(watchdog_log, &prog.name);
    match launcher::launch(prog, Some(&out_log)) {
        Ok(child) => {
            state.child = Some(child);
            // 端口模式下重新计算启动宽限期，等待端口打开
            state.port_down_since = None;
            logger.info(&format!(
                "[{}] 启动成功（输出日志：{}）",
                prog.name,
                out_log.display()
            ));
        }
        Err(e) => {
            logger.error(&format!("[{}] 启动失败：{e}", prog.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Program;
    use std::path::PathBuf;

    /// 回归：skip = true 的程序必须进入跳过列表，未标记的进入监控列表
    /// （曾因 partition 谓词方向写反，导致正式服务只监控被跳过的程序）
    #[test]
    fn split_skipped_separates_monitor_and_skip() {
        let make = |name: &str, skip: bool| Program {
            name: name.to_string(),
            path: PathBuf::from(r"C:\a\a.exe"),
            args: vec![],
            workdir: None,
            java: None,
            detect: None,
            max_restarts_per_hour: 10,
            start_delay_secs: 0,
            skip,
            log_file: None,
        };
        let (programs, skipped) =
            split_skipped(vec![make("normal", false).to_effective(), make("off", true).to_effective()]);
        let program_names: Vec<&str> = programs.iter().map(|p| p.name.as_str()).collect();
        let skipped_names: Vec<&str> = skipped.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(program_names, vec!["normal"]);
        assert_eq!(skipped_names, vec!["off"]);
    }
}
