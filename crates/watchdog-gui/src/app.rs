//! 配置编辑界面：程序列表、编辑表单、全局设置、服务管理与日志查看。
//!
//! 布局使用 egui 0.36 的 `Panel::top/bottom/left` + `CentralPanel`（展示在 App::ui 的根 Ui 中）。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use watchdog_core::config::{
    Config, DetectConfig, EffectiveProgram, Program, DEFAULT_CHECK_INTERVAL_SECS,
    DEFAULT_MAX_RESTARTS_PER_HOUR,
};
use watchdog_core::status::BatchResult;
use watchdog_core::util::{
    default_config_path, exe_dir, pause_flag_path, program_log_path, resolve_log_path,
};

use crate::log_view::LogView;
use crate::service_ctrl::{self, SvcState};

/// 单个程序的手动操作类型
#[derive(Clone, Copy)]
enum ProgramAction {
    Start,
    Stop,
    Restart,
}

/// 日志窗口当前查看的目标
#[derive(Clone, Copy, PartialEq)]
enum LogTarget {
    /// 看门狗自身日志
    Watchdog,
    /// 第 i 个程序的控制台输出日志（logs\<名称>.log）
    Console(usize),
    /// 第 i 个程序配置的日志文件（program.log_file，程序自己写出的业务日志）
    Custom(usize),
}

/// 后台线程发回 UI 线程的结果
enum RefreshMsg {
    /// 服务状态查询结果
    Service(Result<Option<SvcState>, String>),
    /// 各被监控程序的运行状态（与配置顺序一致）
    Running(Vec<bool>),
    /// 一次性后台操作完成：提示消息；`saved` 表示操作包含的配置保存是否成功（是则清除脏标记）
    Action {
        result: Result<String, String>,
        saved: bool,
    },
    /// 打开配置完成：成功时携带新配置与路径
    ConfigLoaded(Result<(Config, PathBuf), String>),
}

/// 状态栏提示
struct StatusMsg {
    text: String,
    is_error: bool,
}

impl StatusMsg {
    fn ok(text: impl Into<String>) -> Self {
        StatusMsg {
            text: text.into(),
            is_error: false,
        }
    }
    fn err(text: impl Into<String>) -> Self {
        StatusMsg {
            text: text.into(),
            is_error: true,
        }
    }
}

pub struct WatchdogApp {
    config: Config,
    config_path: PathBuf,
    dirty: bool,
    selected: Option<usize>,
    status: StatusMsg,
    show_log: bool,
    /// 日志窗口当前查看的目标
    log_target: LogTarget,
    /// 上次加载日志内容的文件（切换查看目标时立即重新加载，不等自动刷新周期）
    log_loaded_for: Option<PathBuf>,
    log_view: LogView,
    service_state: Option<SvcState>,
    service_error: Option<String>,
    is_admin: bool,
    install_exe_path: PathBuf,
    /// 各被监控程序的运行状态（与 config.programs 顺序一致），绿灯=运行/红灯=未运行
    running_flags: Vec<bool>,
    /// 后台刷新线程的结果接收端（UI 线程每帧非阻塞消费）
    refresh_rx: Receiver<RefreshMsg>,
    /// 通道发送端：供一次性后台操作克隆使用
    refresh_tx: Sender<RefreshMsg>,
    /// 与后台刷新线程共享的程序列表（线程每轮扫描前取最新值）
    shared_programs: Arc<Mutex<Vec<EffectiveProgram>>>,
    /// 正在后台执行的操作描述；非 None 时执行类按钮禁用、状态栏显示"执行中"
    busy: Option<String>,
}

impl WatchdogApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::setup_cjk_fonts(&cc.egui_ctx);

        let config_path = default_config_path();
        let (config, status) = match Config::load(&config_path) {
            Ok(c) => (
                c,
                StatusMsg::ok(format!("已加载配置：{}", config_path.display())),
            ),
            Err(e) => (
                Config {
                    check_interval_secs: DEFAULT_CHECK_INTERVAL_SECS,
                    log_file: None,
                    programs: Vec::new(),
                },
                StatusMsg::err(format!("加载配置失败，将从空白配置开始：{e}")),
            ),
        };

        // 后台线程负责周期性重量级刷新（服务状态查询、全系统进程扫描），
        // 避免在 UI 线程上执行导致输入卡顿
        let shared_programs = Arc::new(Mutex::new(Vec::new()));
        let (refresh_tx, refresh_rx) = mpsc::channel();
        spawn_refresh_thread(refresh_tx.clone(), Arc::clone(&shared_programs));

        WatchdogApp {
            config,
            config_path,
            dirty: false,
            selected: None,
            status,
            show_log: false,
            log_target: LogTarget::Watchdog,
            log_loaded_for: None,
            log_view: LogView::default(),
            service_state: None,
            service_error: None,
            is_admin: service_ctrl::is_elevated(),
            install_exe_path: exe_dir().join("watchdog.exe"),
            running_flags: Vec::new(),
            refresh_rx,
            refresh_tx,
            shared_programs,
            busy: None,
        }
    }

    /// 把一次性操作放到后台线程执行（结果经通道回传，完成后 UI 清除 busy）。
    ///
    /// 闭包可用 `tx` 先发送中间状态（如操作后的即时状态查询），并返回最终的
    /// `RefreshMsg::Action`。任何可能阻塞的操作（服务控制、进程扫描、文件 I/O）
    /// 都必须经由本方法执行，禁止直接在 UI 线程调用。
    fn run_in_background<F>(&mut self, busy_label: &str, f: F)
    where
        F: FnOnce(&Sender<RefreshMsg>) -> RefreshMsg + Send + 'static,
    {
        self.busy = Some(busy_label.to_owned());
        let tx = self.refresh_tx.clone();
        std::thread::spawn(move || {
            // 即使闭包异常退出也要回传结果，否则 busy 状态将永久卡住
            let msg = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&tx))) {
                Ok(msg) => msg,
                Err(_) => RefreshMsg::Action {
                    result: Err("后台操作异常退出，请重试".to_owned()),
                    saved: false,
                },
            };
            let _ = tx.send(msg);
        });
    }

    // ---------- 服务 ----------

    /// 服务操作（启动/停止/重启/卸载）：后台线程执行。
    /// 停止/卸载内部会轮询等待服务进入目标状态（最长 30 秒），绝不能在 UI 线程调用。
    fn service_action(
        &mut self,
        action: fn() -> Result<(), String>,
        ok_msg: &'static str,
        busy_label: &'static str,
    ) {
        self.run_in_background(busy_label, move |tx| {
            let result = match action() {
                Ok(()) => {
                    // 操作完成后立即查询一次服务状态，先于结果消息发出（即时反馈）
                    let _ = tx.send(RefreshMsg::Service(service_ctrl::query()));
                    Ok(ok_msg.to_owned())
                }
                Err(e) => Err(e),
            };
            RefreshMsg::Action {
                result,
                saved: false,
            }
        });
    }

    /// 对当前选中的程序执行手动操作（启动/停止/重启）：后台线程执行。
    /// 停止/重启内含全系统进程扫描与最长 10 秒的退出等待，绝不能在 UI 线程调用。
    fn program_action(&mut self, idx: usize, action: ProgramAction) {
        let effective = self.config.programs[idx].to_effective();
        let name = effective.name.clone();
        // 与看门狗服务一致：手动启动/重启同样把程序输出记录到 logs\<名称>.log
        let out_log = program_log_path(&resolve_log_path(&self.config), &name);
        let verb = match action {
            ProgramAction::Start => "启动",
            ProgramAction::Stop => "停止",
            ProgramAction::Restart => "重启",
        };
        let programs = self.config.effective_programs();
        let busy_label = format!("[{name}] 正在{verb}（需等待进程退出）…");
        self.run_in_background(&busy_label, move |tx| {
            let result = match action {
                ProgramAction::Start => {
                    watchdog_core::status::start_program(&effective, Some(&out_log)).map(|_| {
                        format!("[{name}] 已手动启动（输出日志：{}）", out_log.display())
                    })
                }
                ProgramAction::Stop => {
                    watchdog_core::status::stop_program(&effective).map(|n| match n {
                        0 => format!("[{name}] 当前未在运行，无需停止"),
                        n => format!("[{name}] 已终止 {n} 个进程"),
                    })
                }
                ProgramAction::Restart => {
                    watchdog_core::status::restart_program(&effective, Some(&out_log))
                        .map(|msg| format!("[{name}] {msg}"))
                }
            };
            let result = result.map_err(|e| format!("[{name}] {e}"));
            // 操作完成后立即扫描一次运行状态灯，先于结果消息发出（即时反馈）
            let _ = tx.send(RefreshMsg::Running(watchdog_core::status::scan_running(&programs)));
            RefreshMsg::Action {
                result,
                saved: false,
            }
        });
    }

    /// 一键启动所有程序（后台执行）：先移除暂停标记恢复服务的自动拉起，
    /// 再启动所有未跳过且未运行的程序（进程扫描与批量拉起较重，不能在 UI 线程调用）。
    fn start_all(&mut self) {
        let programs = self.config.effective_programs();
        let base_log = resolve_log_path(&self.config);
        let pause_flag = pause_flag_path(&self.config_path);
        self.run_in_background("正在启动所有程序…", move |_tx| {
            // 全部启动即恢复自动拉起：先移除暂停标记（不存在时忽略错误）
            let _ = std::fs::remove_file(&pause_flag);
            let results = watchdog_core::status::start_all_programs(&programs, &base_log);
            // 启动中的进程需要时间出现，状态灯交给后台刷新线程按周期更新
            let (text, has_err) = batch_summary(&results, true);
            RefreshMsg::Action {
                result: if has_err { Err(text) } else { Ok(text) },
                saved: false,
            }
        });
    }

    /// 一键停止所有程序（后台执行）：服务运行中先写暂停标记，避免程序在下个
    /// 检测周期被重新拉起；终止后等待进程退出再刷新状态灯（不能在 UI 线程调用）。
    fn stop_all(&mut self) {
        let programs = self.config.effective_programs();
        let pause_flag = pause_flag_path(&self.config_path);
        let service_running = self.service_state == Some(SvcState::Running);
        self.run_in_background("正在停止所有程序…", move |tx| {
            // 先写暂停标记再终止进程，避免时序上被服务抢先重新拉起
            let mut paused = false;
            let mut pause_warn: Option<String> = None;
            if service_running {
                if let Some(dir) = pause_flag.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::write(&pause_flag, b"") {
                    Ok(()) => paused = true,
                    Err(e) => {
                        pause_warn = Some(format!(
                            "暂停标记写入失败：{e}，服务可能在下个检测周期重新拉起程序"
                        ))
                    }
                }
            }
            let results = watchdog_core::status::stop_all_programs(&programs);
            // 等待匹配进程退出（最长 5 秒）后刷新状态灯，让红灯及时出现
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let snapshot = watchdog_core::status::Snapshot::scan();
                if programs
                    .iter()
                    .all(|p| p.skip || snapshot.matching_pids(p).is_empty())
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let _ = tx.send(RefreshMsg::Running(watchdog_core::status::scan_running(&programs)));
            let (mut text, has_err) = batch_summary(&results, false);
            if paused {
                text.push_str("；自动拉起已暂停，点「全部启动」恢复");
            }
            let mut pause_failed = false;
            if let Some(warn) = pause_warn {
                text.push_str(&format!("；{warn}"));
                pause_failed = true;
            }
            RefreshMsg::Action {
                result: if has_err || pause_failed {
                    Err(text)
                } else {
                    Ok(text)
                },
                saved: false,
            }
        });
    }

    /// 尽力移除暂停标记：服务启动/重启/安装是显式恢复监控的操作，随之恢复自动拉起
    fn resume_auto_start(&self) {
        let _ = std::fs::remove_file(pause_flag_path(&self.config_path));
    }

    fn install_service(&mut self) {
        if !self.install_exe_path.exists() {
            match rfd::FileDialog::new()
                .set_title("选择 watchdog.exe")
                .add_filter("watchdog 程序", &["exe"])
                .pick_file()
            {
                Some(p) => self.install_exe_path = p,
                None => {
                    self.status = StatusMsg::err("已取消安装：未选择 watchdog.exe");
                    return;
                }
            }
        }
        // 保存配置 + 安装（安装内部要等待旧服务卸载/停止，最长可达数十秒）整体后台执行
        // 安装即恢复监控：顺带移除暂停标记
        self.resume_auto_start();
        let exe = self.install_exe_path.clone();
        let config = self.config.clone();
        let config_path = self.config_path.clone();
        self.run_in_background("正在安装并启动服务（含保存配置）…", move |tx| {
            let result = match config.save(&config_path) {
                Ok(()) => match service_ctrl::install(&exe, &config_path) {
                    Ok(()) => {
                        let _ = tx.send(RefreshMsg::Service(service_ctrl::query()));
                        Ok("服务已安装并启动（开机自启已启用）".to_owned())
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(format!("保存失败：{e}")),
            };
            RefreshMsg::Action {
                saved: result.is_ok(),
                result,
            }
        });
    }

    // ---------- 配置 ----------

    /// 后台保存当前配置（校验 + 写盘不阻塞 UI；成功后清除脏标记）
    fn save_config(&mut self) {
        let config = self.config.clone();
        let path = self.config_path.clone();
        self.run_in_background("正在保存配置…", move |_tx| {
            let result = config
                .save(&path)
                .map(|_| format!("已保存：{}", path.display()))
                .map_err(|e| format!("保存失败：{e}"));
            RefreshMsg::Action {
                saved: result.is_ok(),
                result,
            }
        });
    }

    fn open_config_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("TOML 配置", &["toml"])
            .pick_file()
        {
            // 读盘 + TOML 解析放到后台执行，不阻塞 UI
            self.run_in_background("正在读取配置…", move |_tx| {
                RefreshMsg::ConfigLoaded(
                    Config::load(&p)
                        .map(|c| (c, p))
                        .map_err(|e| format!("打开失败：{e}")),
                )
            });
        }
    }

    fn save_as_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("TOML 配置", &["toml"])
            .set_file_name("watchdog.toml")
            .save_file()
        {
            self.config_path = p;
            self.save_config();
        }
    }

    fn add_program(&mut self) {
        self.config.programs.push(Program {
            name: format!("新程序{}", self.config.programs.len() + 1),
            path: PathBuf::new(),
            args: Vec::new(),
            workdir: None,
            java: None,
            detect: None,
            max_restarts_per_hour: DEFAULT_MAX_RESTARTS_PER_HOUR,
            start_delay_secs: 0,
            skip: false,
            log_file: None,
        });
        self.selected = Some(self.config.programs.len() - 1);
        self.dirty = true;
        self.status = StatusMsg::ok("已添加新程序，请在右侧填写路径");
    }

    // ---------- 界面 ----------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("配置文件:");
                ui.label(
                    egui::RichText::new(self.config_path.display().to_string())
                        .color(egui::Color32::from_rgb(0x55, 0x80, 0xC0)),
                );
                // 有后台操作执行中时，一次性操作按钮全部禁用，防止并发冲突
                let idle = self.busy.is_none();
                if ui
                    .add_enabled(idle, egui::Button::new("打开…"))
                    .clicked()
                {
                    self.open_config_dialog();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("另存为…"))
                    .clicked()
                {
                    self.save_as_dialog();
                }
                let save_label = if self.dirty { "保存 *" } else { "保存" };
                if ui
                    .add_enabled(idle, egui::Button::new(save_label))
                    .clicked()
                {
                    self.save_config();
                }
                ui.separator();

                // 服务状态
                match &self.service_state {
                    Some(SvcState::Running) => {
                        ui.colored_label(egui::Color32::from_rgb(0x2E, 0xA0, 0x44), "服务: 运行中");
                    }
                    Some(state) => {
                        ui.colored_label(egui::Color32::GRAY, format!("服务: {}", state.label()));
                    }
                    None => {
                        ui.colored_label(egui::Color32::GRAY, "服务: —");
                    }
                }
                if let Some(err) = &self.service_error {
                    ui.colored_label(egui::Color32::from_rgb(0xC0, 0x70, 0x00), err);
                }

                if ui
                    .add_enabled(self.is_admin && idle, egui::Button::new("安装"))
                    .on_hover_text("安装并启动 Windows 服务（需先保存配置；开机自启）")
                    .clicked()
                {
                    self.install_service();
                }
                if ui
                    .add_enabled(self.is_admin && idle, egui::Button::new("启动"))
                    .on_hover_text("启动服务，并恢复被暂停的自动拉起")
                    .clicked()
                {
                    self.resume_auto_start();
                    self.service_action(service_ctrl::start_service, "服务已启动", "正在启动服务…");
                }
                if ui
                    .add_enabled(self.is_admin && idle, egui::Button::new("停止"))
                    .on_hover_text("停止服务需等待其完全退出（最长 30 秒）")
                    .clicked()
                {
                    self.service_action(
                        service_ctrl::stop_service,
                        "服务已停止",
                        "正在停止服务（最长等待 30 秒）…",
                    );
                }
                if ui
                    .add_enabled(self.is_admin && idle, egui::Button::new("重启"))
                    .on_hover_text("重启服务使最新配置生效，并恢复被暂停的自动拉起")
                    .clicked()
                {
                    self.resume_auto_start();
                    self.service_action(service_ctrl::restart_service, "服务已重启", "正在重启服务…");
                }
                if ui
                    .add_enabled(self.is_admin && idle, egui::Button::new("卸载"))
                    .on_hover_text("卸载服务需等待其完全退出（最长 30 秒）")
                    .clicked()
                {
                    self.service_action(
                        service_ctrl::delete_service,
                        "服务已卸载",
                        "正在卸载服务（最长等待 30 秒）…",
                    );
                }
                ui.separator();
                ui.checkbox(&mut self.show_log, "日志");
            });
            // 状态栏
            ui.horizontal(|ui| {
                if let Some(label) = &self.busy {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xC0, 0x70, 0x00),
                        format!("⏳ {label}"),
                    );
                    ui.separator();
                }
                let color = if self.status.is_error {
                    egui::Color32::from_rgb(0xC0, 0x30, 0x30)
                } else {
                    egui::Color32::GRAY
                };
                ui.colored_label(color, &self.status.text);
                if self.dirty {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xC0, 0x70, 0x00),
                        "（有未保存的修改）",
                    );
                }
            });
            ui.add_space(4.0);
        });
    }

    fn admin_banner(&self, ui: &mut egui::Ui) {
        egui::Panel::top("admin_banner").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(0xC0, 0x70, 0x00),
                    "当前未以管理员身份运行：可编辑配置，但服务管理功能不可用。",
                );
                if ui.button("以管理员身份重启").clicked() {
                    if !service_ctrl::relaunch_as_admin() {
                        eprintln!("无法发起提权重启");
                    }
                }
            });
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("program_list")
            .resizable(true)
            .default_size(330.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("被监控程序");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("＋ 新增").clicked() {
                            self.add_program();
                        }
                    });
                });
                ui.separator();
                // 状态图例
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 3.5, egui::Color32::from_rgb(0x2E, 0xA0, 0x44));
                    ui.label(egui::RichText::new("运行中").weak().small());
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 3.5, egui::Color32::from_rgb(0xC8, 0x38, 0x38));
                    ui.label(egui::RichText::new("未运行").weak().small());
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 3.5, egui::Color32::GRAY);
                    ui.label(egui::RichText::new("已跳过").weak().small());
                    ui.label(
                        egui::RichText::new("（每 3 秒刷新；检测规则与服务端一致）")
                            .weak()
                            .small(),
                    );
                });

                // 批量操作：一键全部启动 / 全部停止（已停用 skip 的程序不受影响）
                ui.horizontal(|ui| {
                    let idle = self.busy.is_none();
                    if ui
                        .add_enabled(idle, egui::Button::new("▶ 全部启动"))
                        .on_hover_text("启动所有未运行的程序（已停用 skip 的除外），并恢复服务的自动拉起")
                        .clicked()
                    {
                        self.start_all();
                    }
                    if ui
                        .add_enabled(idle, egui::Button::new("■ 全部停止"))
                        .on_hover_text("停止所有程序（已停用 skip 的除外）；服务运行中时会自动暂停自动拉起，避免被重新拉起，点「全部启动」恢复")
                        .clicked()
                    {
                        self.stop_all();
                    }
                    if pause_flag_path(&self.config_path).exists() {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xC0, 0x70, 0x00),
                            "⏸ 自动拉起已暂停",
                        )
                        .on_hover_text("看门狗服务已暂停自动拉起（「全部停止」所致），已运行的程序保持不变；点「全部启动」或服务「启动/重启」恢复");
                    }
                });
                ui.separator();

                let mut clicked: Option<usize> = None;
                let mut remove: Option<usize> = None;
                let mut action_req: Option<(usize, ProgramAction)> = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, prog) in self.config.programs.iter().enumerate() {
                            let ext = extension_of(&prog.path);
                            let running = self.running_flags.get(i).copied().unwrap_or(false);
                            let skip_note = if prog.skip { " · 已跳过" } else { "" };
                            let mut text = egui::RichText::new(format!(
                                "{}\n{} · {}{}",
                                prog.name,
                                type_label(&ext),
                                short_path(&prog.path),
                                skip_note
                            ));
                            if prog.skip {
                                text = text.weak();
                            }
                            let selected = self.selected == Some(i);
                            ui.horizontal(|ui| {
                                // 状态指示灯：绿=运行，红=未运行，灰=已跳过（跳过优先显示）
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                                let color = if prog.skip {
                                    egui::Color32::GRAY
                                } else if running {
                                    egui::Color32::from_rgb(0x2E, 0xA0, 0x44)
                                } else {
                                    egui::Color32::from_rgb(0xC8, 0x38, 0x38)
                                };
                                ui.painter().circle_filled(rect.center(), 4.5, color);
                                let response = ui.selectable_label(selected, text);
                                let response = response.on_hover_text(if prog.skip {
                                    "已跳过：看门狗不监控、不自动拉起该程序（手动启停仍可用）"
                                } else if running {
                                    "运行中"
                                } else {
                                    "未运行（看门狗服务会在下个检测周期自动拉起）"
                                });
                                if response.clicked() {
                                    clicked = Some(i);
                                }
                                let idle = self.busy.is_none();
                                response.context_menu(|ui| {
                                    if ui
                                        .add_enabled(idle, egui::Button::new("启动"))
                                        .clicked()
                                    {
                                        action_req = Some((i, ProgramAction::Start));
                                        ui.close();
                                    }
                                    if ui
                                        .add_enabled(idle, egui::Button::new("停止"))
                                        .clicked()
                                    {
                                        action_req = Some((i, ProgramAction::Stop));
                                        ui.close();
                                    }
                                    if ui
                                        .add_enabled(idle, egui::Button::new("重启"))
                                        .clicked()
                                    {
                                        action_req = Some((i, ProgramAction::Restart));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button("删除").clicked() {
                                        remove = Some(i);
                                        ui.close();
                                    }
                                });
                            });
                        }
                        if self.config.programs.is_empty() {
                            ui.label(
                                egui::RichText::new("暂无程序，点击右上角「＋ 新增」添加").weak(),
                            );
                        }
                    });

                if let Some(i) = clicked {
                    self.selected = Some(i);
                }
                if let Some((i, action)) = action_req {
                    self.program_action(i, action);
                }
                if let Some(i) = remove {
                    self.config.programs.remove(i);
                    self.selected = match self.selected {
                        Some(s) if s == i => None,
                        Some(s) if s > i => Some(s - 1),
                        other => other,
                    };
                    self.dirty = true;
                }
            });
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some(idx) = self.selected else {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new("在左侧选择程序进行编辑，或点击「＋ 新增」")
                            .weak()
                            .size(16.0),
                    );
                });
                return;
            };

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut delete = false;
                    let mut action_req: Option<ProgramAction> = None;
                    let mut open_log_req = false;
                    let dirty_local;
                    {
                        let prog = &mut self.config.programs[idx];
                        dirty_local = program_form(ui, prog, &mut delete);
                    }
                    self.dirty |= dirty_local;
                    if !delete {
                        // 单程序手动操作
                        ui.separator();
                        let idle = self.busy.is_none();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(idle, egui::Button::new("▶ 启动"))
                                .on_hover_text("立即启动该程序（不影响看门狗的自动监控）")
                                .clicked()
                            {
                                action_req = Some(ProgramAction::Start);
                            }
                            if ui
                                .add_enabled(idle, egui::Button::new("■ 停止"))
                                .on_hover_text("终止该程序的所有匹配进程（按检测规则匹配）")
                                .clicked()
                            {
                                action_req = Some(ProgramAction::Stop);
                            }
                            if ui
                                .add_enabled(idle, egui::Button::new("↻ 重启"))
                                .on_hover_text("先停止再启动（等待进程退出最长 10 秒）")
                                .clicked()
                            {
                                action_req = Some(ProgramAction::Restart);
                            }
                            if ui
                                .button("📄 日志")
                                .on_hover_text("打开日志窗口实时查看该程序日志：配置了「日志文件」时打开之，否则打开控制台输出（logs\\<名称>.log）")
                                .clicked()
                            {
                                open_log_req = true;
                            }
                            ui.label(
                                egui::RichText::new("手动操作仅对当前生效；看门狗服务运行中时，停止后的程序会在下个检测周期被自动拉起")
                                    .weak()
                                    .small(),
                            );
                        });
                    }
                    if let Some(action) = action_req {
                        self.program_action(idx, action);
                    }
                    if open_log_req {
                        // 配置了日志文件的程序直接打开业务日志，否则打开控制台输出
                        self.log_target = match self.config.programs[idx].log_file {
                            Some(_) => LogTarget::Custom(idx),
                            None => LogTarget::Console(idx),
                        };
                        self.show_log = true;
                    }
                    if delete {
                        self.config.programs.remove(idx);
                        self.selected = None;
                        self.dirty = true;
                    }
                });
        });
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("global_settings").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("检测间隔:");
                let mut interval = self.config.check_interval_secs;
                if ui
                    .add(egui::DragValue::new(&mut interval).range(1..=3600u64))
                    .changed()
                {
                    self.config.check_interval_secs = interval;
                    self.dirty = true;
                }
                ui.label("秒");
                ui.separator();

                ui.label("日志文件:");
                let mut log = self
                    .config
                    .log_file
                    .clone()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut log)
                            .desired_width(280.0)
                            .hint_text("留空 = 程序目录\\logs\\watchdog.log"),
                    )
                    .changed()
                {
                    self.config.log_file = if log.trim().is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(log))
                    };
                    self.dirty = true;
                }
                if ui.button("浏览…").clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .add_filter("日志", &["log"])
                        .pick_file()
                    {
                        self.config.log_file = Some(f);
                        self.dirty = true;
                    }
                }
                if ui.button("默认").clicked() {
                    self.config.log_file = None;
                    self.dirty = true;
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "生效日志路径：{}",
                        resolve_log_path(&self.config).display()
                    ))
                    .weak(),
                );
            });
            ui.add_space(4.0);
        });
    }

    /// 日志窗口当前查看目标的日志路径；目标越界（程序被删除）时回落到看门狗日志
    fn current_log_path(&self) -> PathBuf {
        let watchdog_log = resolve_log_path(&self.config);
        match self.log_target {
            LogTarget::Watchdog => watchdog_log,
            LogTarget::Console(i) if i < self.config.programs.len() => {
                program_log_path(&watchdog_log, &self.config.programs[i].name)
            }
            LogTarget::Custom(i) if i < self.config.programs.len() => self.config.programs[i]
                .log_file
                .clone()
                .unwrap_or(watchdog_log),
            _ => watchdog_log,
        }
    }

    /// 日志窗口下拉框显示的当前目标文本
    fn log_target_label(&self) -> String {
        match self.log_target {
            LogTarget::Watchdog => "看门狗日志".to_owned(),
            LogTarget::Console(i) => self
                .config
                .programs
                .get(i)
                .map(|p| format!("程序：{}（控制台）", p.name))
                .unwrap_or_else(|| "看门狗日志".to_owned()),
            LogTarget::Custom(i) => self
                .config
                .programs
                .get(i)
                .map(|p| format!("程序：{}（日志文件）", p.name))
                .unwrap_or_else(|| "看门狗日志".to_owned()),
        }
    }

    fn log_window(&mut self, ui: &mut egui::Ui) {
        if !self.show_log {
            return;
        }
        // 选中程序被删除后索引越界 → 回落到看门狗日志
        match self.log_target {
            LogTarget::Watchdog => {}
            LogTarget::Console(i) | LogTarget::Custom(i) => {
                if i >= self.config.programs.len() {
                    self.log_target = LogTarget::Watchdog;
                }
            }
        }
        let ctx = ui.ctx().clone();
        let mut open = self.show_log;
        egui::Window::new("日志查看")
            .open(&mut open)
            .default_size([820.0, 320.0])
            .show(&ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("查看:");
                    let mut switched = false;
                    egui::ComboBox::from_id_salt("log_target")
                        .selected_text(self.log_target_label())
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_value(
                                    &mut self.log_target,
                                    LogTarget::Watchdog,
                                    "看门狗日志",
                                )
                                .changed()
                            {
                                switched = true;
                            }
                            for (i, prog) in self.config.programs.iter().enumerate() {
                                if ui
                                    .selectable_value(
                                        &mut self.log_target,
                                        LogTarget::Console(i),
                                        format!("程序：{}（控制台）", prog.name),
                                    )
                                    .changed()
                                {
                                    switched = true;
                                }
                                // 配置了日志文件的程序：追加「日志文件」条目查看其业务日志
                                if prog.log_file.is_some()
                                    && ui
                                        .selectable_value(
                                            &mut self.log_target,
                                            LogTarget::Custom(i),
                                            format!("程序：{}（日志文件）", prog.name),
                                        )
                                        .changed()
                                {
                                    switched = true;
                                }
                            }
                        });
                    if switched {
                        // 切换目标后立即加载，不等自动刷新周期
                        let path = self.current_log_path();
                        self.log_view.load(&path);
                        self.log_loaded_for = Some(path);
                    }
                });
                let path = self.current_log_path();
                self.log_view.ui(ui, &path);
            });
        self.show_log = open;
    }
}

impl eframe::App for WatchdogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 应用后台线程发回的刷新结果（非阻塞；所有重量级操作都在后台线程执行）
        while let Ok(msg) = self.refresh_rx.try_recv() {
            match msg {
                RefreshMsg::Service(result) => match result {
                    Ok(state) => {
                        self.service_state = Some(state.unwrap_or(SvcState::NotInstalled));
                        self.service_error = None;
                    }
                    Err(e) => {
                        self.service_state = None;
                        self.service_error = Some(e);
                    }
                },
                RefreshMsg::Running(flags) => self.running_flags = flags,
                RefreshMsg::Action { result, saved } => {
                    self.busy = None;
                    if saved {
                        self.dirty = false;
                    }
                    self.status = match result {
                        Ok(text) => StatusMsg::ok(text),
                        Err(text) => StatusMsg::err(text),
                    };
                }
                RefreshMsg::ConfigLoaded(Ok((config, path))) => {
                    self.busy = None;
                    self.config = config;
                    self.config_path = path;
                    self.dirty = false;
                    self.selected = None;
                    self.status = StatusMsg::ok("配置已打开");
                }
                RefreshMsg::ConfigLoaded(Err(e)) => {
                    self.busy = None;
                    self.status = StatusMsg::err(e);
                }
            }
        }
        // 把最新程序列表同步给后台线程（下一次进程扫描使用）
        if let Ok(mut progs) = self.shared_programs.lock() {
            *progs = self.config.effective_programs();
        }
        if self.show_log {
            // 查看目标变化时立即加载新日志，否则按自动刷新周期读取尾部（读取在后台线程）
            let path = self.current_log_path();
            if self.log_loaded_for.as_ref() != Some(&path) {
                self.log_view.load(&path);
                self.log_loaded_for = Some(path.clone());
            } else {
                self.log_view.refresh(&path);
            }
            self.log_view.poll(&path);
        }
        // 有操作在后台执行时加快重绘，完成提示能及时出现
        let repaint = if self.busy.is_some() {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(1)
        };
        ui.ctx().request_repaint_after(repaint);

        self.top_bar(ui);
        if !self.is_admin {
            self.admin_banner(ui);
        }
        self.left_panel(ui);
        self.central_panel(ui);
        self.bottom_panel(ui);
        self.log_window(ui);
    }
}

/// 后台刷新线程：每 2 秒查询服务状态、每 3 秒扫描程序运行状态，结果经通道发回 UI。
///
/// 全系统进程快照（Snapshot::scan）与 SCM 查询在 Windows 上耗时可达数百毫秒，
/// 必须在后台线程执行；UI 线程每帧 try_recv 应用结果，输入不再被阻塞。
fn spawn_refresh_thread(
    tx: Sender<RefreshMsg>,
    programs: Arc<Mutex<Vec<EffectiveProgram>>>,
) {
    std::thread::spawn(move || {
        // 置为早已到期：线程启动后立即执行首轮刷新
        let mut last_service = Instant::now() - Duration::from_secs(60);
        let mut last_scan = Instant::now() - Duration::from_secs(60);
        loop {
            if last_service.elapsed() >= Duration::from_secs(2) {
                last_service = Instant::now();
                if tx.send(RefreshMsg::Service(service_ctrl::query())).is_err() {
                    return; // UI 已关闭（接收端丢弃）
                }
            }
            if last_scan.elapsed() >= Duration::from_secs(3) {
                last_scan = Instant::now();
                let snapshot_programs = programs.lock().map(|p| p.clone()).unwrap_or_default();
                let flags = watchdog_core::status::scan_running(&snapshot_programs);
                if tx.send(RefreshMsg::Running(flags)).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

/// 单个程序的编辑表单；返回是否有修改。
fn program_form(ui: &mut egui::Ui, prog: &mut Program, delete_clicked: &mut bool) -> bool {
    let mut dirty = false;

    egui::Grid::new("program_form")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("名称:");
            dirty |= ui
                .text_edit_singleline(&mut prog.name)
                .on_hover_text("唯一名称，用于日志与状态跟踪")
                .changed();
            ui.end_row();

            ui.label("跳过监控:");
            dirty |= ui
                .checkbox(&mut prog.skip, "")
                .on_hover_text("勾选后看门狗不监控、不自动拉起该程序（配置保留，取消勾选即可恢复；保存后需重启服务生效）")
                .changed();
            ui.label(
                egui::RichText::new("勾选后不监控、不自动拉起该程序，配置保留；保存后需重启服务生效")
                    .weak(),
            );
            ui.end_row();

            // 目录 + 文件名 → 合成 path
            let mut dir = parent_dir(&prog.path);
            ui.label("目录:");
            ui.horizontal(|ui| {
                dirty |= ui
                    .add(egui::TextEdit::singleline(&mut dir).desired_width(300.0))
                    .changed();
                if ui.button("浏览…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        dir = d.to_string_lossy().into_owned();
                        dirty = true;
                    }
                }
            });
            ui.end_row();

            ui.label("文件名:");
            let ext = extension_of(&prog.path);
            let filters: &[&str] = match ext.as_str() {
                "exe" => &["exe"],
                "bat" | "cmd" => &["bat", "cmd"],
                "jar" => &["jar"],
                "ps1" => &["ps1"],
                _ => &["exe", "bat", "cmd", "jar", "ps1"],
            };
            let mut file = file_name_of(&prog.path);
            ui.horizontal(|ui| {
                dirty |= ui
                    .add(egui::TextEdit::singleline(&mut file).desired_width(220.0))
                    .changed();
                if ui.button("浏览…").clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .add_filter("程序文件", filters)
                        .pick_file()
                    {
                        dir = parent_dir(&f);
                        file = file_name_of(&f);
                        dirty = true;
                    }
                }
            });
            let joined = PathBuf::from(&dir).join(&file);
            if joined != prog.path {
                prog.path = joined;
                dirty = true;
            }
            ui.end_row();

            let new_ext = extension_of(&prog.path);
            ui.label("类型:");
            ui.label(
                egui::RichText::new(format!("{}（由文件扩展名决定）", type_label(&new_ext))).weak(),
            );
            ui.end_row();

            ui.label("工作目录:");
            let mut workdir = prog
                .workdir
                .clone()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut workdir)
                            .desired_width(300.0)
                            .hint_text("留空 = 程序所在目录"),
                    )
                    .changed();
                if ui.button("浏览…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        workdir = d.to_string_lossy().into_owned();
                        dirty = true;
                    }
                }
            });
            if workdir.trim().is_empty() {
                prog.workdir = None;
            } else {
                prog.workdir = Some(PathBuf::from(workdir));
            }
            ui.end_row();

            // 程序自己写出的日志文件（可选）：仅供日志窗口实时查看，看门狗不读写
            ui.label("日志文件:").on_hover_text(
                "程序自己写出的日志文件路径（如 Java 应用的 logs\\app.log）。\n\
                 配置后可在日志窗口下拉框中选择「（日志文件）」实时查看；留空则日志窗口仅显示控制台输出。",
            );
            let mut log_file = prog
                .log_file
                .clone()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut log_file)
                            .desired_width(300.0)
                            .hint_text("留空 = 仅控制台输出（logs\\<名称>.log）"),
                    )
                    .changed();
                if ui.button("浏览…").clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .add_filter("日志文件", &["log", "txt", "out"])
                        .add_filter("所有文件", &["*"])
                        .pick_file()
                    {
                        log_file = f.to_string_lossy().into_owned();
                        dirty = true;
                    }
                }
            });
            prog.log_file = if log_file.trim().is_empty() {
                None
            } else {
                Some(PathBuf::from(log_file))
            };
            ui.end_row();

            // jar 专属：java.exe 路径
            if new_ext == "jar" {
                ui.label("Java 路径:");
                let mut java = prog
                    .java
                    .clone()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    dirty |= ui
                        .add(
                            egui::TextEdit::singleline(&mut java)
                                .desired_width(300.0)
                                .hint_text("留空 = JAVA_HOME / PATH 自动解析"),
                        )
                        .changed();
                    if ui.button("浏览…").clicked() {
                        if let Some(f) = rfd::FileDialog::new()
                            .add_filter("java 程序", &["exe"])
                            .pick_file()
                        {
                            java = f.to_string_lossy().into_owned();
                            dirty = true;
                        }
                    }
                });
                if java.trim().is_empty() {
                    prog.java = None;
                } else {
                    prog.java = Some(PathBuf::from(java));
                }
                ui.end_row();
            }

            ui.label("重启上限:");
            dirty |= ui
                .add(egui::DragValue::new(&mut prog.max_restarts_per_hour).range(0..=1000u32))
                .changed();
            ui.label(egui::RichText::new("次/小时（0 = 不限制，防止崩溃循环）").weak());
            ui.end_row();

            ui.label("启动延迟:");
            dirty |= ui
                .add(
                    egui::DragValue::new(&mut prog.start_delay_secs)
                        .range(0..=86400u64)
                        .suffix(" 秒"),
                )
                .changed();
            ui.label(
                egui::RichText::new("看门狗启动后等待 N 秒才首次拉起（控制启动顺序，如先服务后模型）")
                    .weak(),
            );
            ui.end_row();
        });

    // 启动参数（每行一个，多行编辑）
    ui.label("启动参数（每行一个）:");
    let mut args_text = prog.args.join("\n");
    if ui
        .add(
            egui::TextEdit::multiline(&mut args_text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("每行一个参数，如：\n--port\n8080"),
        )
        .changed()
    {
        prog.args = args_text
            .lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect();
        dirty = true;
    }

    // 高级检测规则
    ui.collapsing("高级检测规则（留空则按类型自动推导）", |ui| {
        let mut image = prog
            .detect
            .as_ref()
            .and_then(|d| d.image.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("进程镜像名:");
            dirty |= ui
                .add(
                    egui::TextEdit::singleline(&mut image)
                        .desired_width(200.0)
                        .hint_text("如 java.exe"),
                )
                .changed();
        });
        let mut cmdline = prog
            .detect
            .as_ref()
            .and_then(|d| d.cmdline_contains.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("命令行包含:");
            dirty |= ui
                .add(
                    egui::TextEdit::singleline(&mut cmdline)
                        .desired_width(320.0)
                        .hint_text(r"如 C:\apps\app.jar"),
                )
                .changed();
        });

        // 端口检测（可选）：配置后以端口可连通判定运行，假死自动恢复
        let mut port_on = prog.detect.as_ref().and_then(|d| d.port).is_some();
        let mut port: u16 = prog.detect.as_ref().and_then(|d| d.port).unwrap_or(8080);
        let mut host = prog
            .detect
            .as_ref()
            .and_then(|d| d.host.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("端口检测:");
            dirty |= ui
                .checkbox(&mut port_on, "启用")
                .on_hover_text("端口可连通才算运行中；进程在但端口持续 60 秒不通将判定假死，自动结束并重新拉起")
                .changed();
            if port_on {
                dirty |= ui
                    .add(egui::DragValue::new(&mut port).range(1..=65535))
                    .changed();
                ui.label("主机:");
                dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut host)
                            .desired_width(120.0)
                            .hint_text("127.0.0.1"),
                    )
                    .on_hover_text("端口检测的主机，缺省 127.0.0.1；服务监听 0.0.0.0 时无需修改")
                    .changed();
            }
        });
        if port_on {
            ui.label(
                egui::RichText::new(
                    "启用后以端口为准：假死判定时会按上述镜像名/命令行规则结束匹配进程，请确保规则能匹配到实际服务进程",
                )
                .weak()
                .small(),
            );
        }

        let image_opt = if image.trim().is_empty() {
            None
        } else {
            Some(image)
        };
        let cmdline_opt = if cmdline.trim().is_empty() {
            None
        } else {
            Some(cmdline)
        };
        let port_opt = if port_on { Some(port) } else { None };
        let host_opt = if port_on && !host.trim().is_empty() {
            Some(host)
        } else {
            None
        };
        let eff_port = match port_opt {
            Some(p) => format!("{}（以此为准）", host_display(&host_opt, p)),
            None => "（未启用）".to_string(),
        };
        if image_opt.is_none() && cmdline_opt.is_none() && port_opt.is_none() {
            prog.detect = None;
        } else {
            prog.detect = Some(DetectConfig {
                image: image_opt,
                cmdline_contains: cmdline_opt,
                port: port_opt,
                host: host_opt,
            });
        }
        let (eff_image, eff_cmdline) = prog.effective_detect();
        ui.label(
            egui::RichText::new(format!(
                "当前生效：镜像名 = {eff_image}；命令行包含 = {}；端口检测 = {eff_port}",
                eff_cmdline.as_deref().unwrap_or("（无）")
            ))
            .weak(),
        );
    });

    ui.separator();
    if ui
        .button(
            egui::RichText::new("删除该程序").color(egui::Color32::from_rgb(0xC0, 0x30, 0x30)),
        )
        .clicked()
    {
        *delete_clicked = true;
    }

    dirty
}

// ---------- 小工具 ----------

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn type_label(ext: &str) -> &'static str {
    match ext {
        "exe" => "exe 程序",
        "bat" | "cmd" => "批处理脚本",
        "jar" => "jar 程序",
        "ps1" => "PowerShell 脚本",
        _ => "其它",
    }
}

fn parent_dir(path: &Path) -> String {
    path.parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn short_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars().count() > 42 {
        let tail: String = s.chars().skip(s.chars().count() - 39).collect();
        format!("…{tail}")
    } else {
        s.into_owned()
    }
}

/// 端口检测目标显示（主机缺省 127.0.0.1）
fn host_display(host: &Option<String>, port: u16) -> String {
    format!(
        "{}:{port}",
        host.as_deref().unwrap_or(watchdog_core::config::DEFAULT_DETECT_HOST)
    )
}

/// 汇总批量全部启动 / 全部停止的结果为状态栏文本（含失败明细）。
///
/// 返回 (文本, 是否有失败)；`is_start` 区分「全部启动 / 全部停止」的措辞与计数口径。
fn batch_summary(results: &[BatchResult], is_start: bool) -> (String, bool) {
    let skipped = results.iter().filter(|r| r.skipped).count();
    let failed: Vec<&BatchResult> = results.iter().filter(|r| r.error.is_some()).collect();
    let mut text = if is_start {
        let started = results
            .iter()
            .filter(|r| !r.skipped && r.error.is_none())
            .count();
        format!("全部启动完成：新启动 {started} 个，跳过 {skipped} 个")
    } else {
        let killed: usize = results
            .iter()
            .filter(|r| r.error.is_none())
            .map(|r| r.count)
            .sum();
        format!("已停止所有程序：终止 {killed} 个进程，跳过 {skipped} 个")
    };
    if !failed.is_empty() {
        text.push_str(&format!("，失败 {} 个", failed.len()));
        for r in &failed {
            text.push_str(&format!(
                "\n[{}] {}",
                r.name,
                r.error.as_deref().unwrap_or_default()
            ));
        }
    }
    (text, !failed.is_empty())
}
