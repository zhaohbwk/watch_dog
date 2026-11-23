//! 配置文件（TOML）的解析、校验与检测规则推导。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 默认检测间隔（秒）
pub const DEFAULT_CHECK_INTERVAL_SECS: u64 = 5;
/// 默认一小时重启上限
pub const DEFAULT_MAX_RESTARTS_PER_HOUR: u32 = 10;
/// 端口检测的默认主机
pub const DEFAULT_DETECT_HOST: &str = "127.0.0.1";

/// 看门狗全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 检测间隔（秒），默认 5，最小 1
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,

    /// 日志文件路径；缺省为 exe 同目录 logs\watchdog.log
    #[serde(default)]
    pub log_file: Option<PathBuf>,

    /// 被监控程序列表
    pub programs: Vec<Program>,
}

/// 单个被监控程序（原始配置项）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Program {
    /// 唯一名称（用于日志与状态跟踪）
    pub name: String,
    /// 程序路径：exe / bat / cmd / jar / ps1
    pub path: PathBuf,
    /// 启动参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 工作目录
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// jar 程序指定的 java.exe 路径；缺省依次取 JAVA_HOME\bin\java.exe、PATH 中的 java.exe
    #[serde(default)]
    pub java: Option<PathBuf>,
    /// 自定义检测规则（覆盖默认推导，未填写的部分保留默认）
    #[serde(default)]
    pub detect: Option<DetectConfig>,
    /// 一小时内最多自动启动次数，防止崩溃循环；0 表示不限制（默认 10）
    #[serde(default = "default_max_restarts")]
    pub max_restarts_per_hour: u32,
    /// 启动延迟（秒）：看门狗启动后等待该秒数才允许首次拉起该程序，
    /// 用于控制多个程序的启动顺序（如先启动服务端、再启动依赖它的模型）；默认 0。
    /// 只约束看门狗启动后的首次拉起；程序退出后的自动重启不受此延迟限制。
    #[serde(default)]
    pub start_delay_secs: u64,
    /// 跳过监控：true 时看门狗不监控、不自动拉起该程序（配置保留，可随时重新启用）；
    /// GUI 中的手动启动/停止不受影响。默认 false。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip: bool,
    /// 程序自己写出的日志文件路径（可选）：仅供 GUI 日志窗口实时查看，
    /// 看门狗不读取、不轮转该文件。缺省时日志窗口查看控制台输出（logs\<名称>.log）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<PathBuf>,
}

/// 自定义检测规则
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DetectConfig {
    /// 按进程镜像名匹配（如 "java.exe"、"legacy-worker.exe"）
    pub image: Option<String>,
    /// 按命令行子串匹配（如 jar/bat 的完整路径），大小写不敏感
    pub cmdline_contains: Option<String>,
    /// 端口检测：配置后以"端口可连通"判定程序是否在运行（进程在但端口不通视为假死）
    #[serde(default)]
    pub port: Option<u16>,
    /// 端口检测的主机（IP 或主机名），缺省 127.0.0.1；需与 detect.port 一起使用
    #[serde(default)]
    pub host: Option<String>,
}

/// 校验并推导检测规则后的程序条目
#[derive(Debug, Clone)]
pub struct EffectiveProgram {
    pub name: String,
    pub path: PathBuf,
    pub args: Vec<String>,
    pub workdir: Option<PathBuf>,
    pub java: Option<PathBuf>,
    pub max_restarts_per_hour: u32,
    /// 启动延迟（秒），见 Program::start_delay_secs
    pub start_delay_secs: u64,
    /// 跳过监控，见 Program::skip
    pub skip: bool,
    /// 进程镜像名（小写）
    pub detect_image: String,
    /// 命令行子串（小写，可选）
    pub detect_cmdline: Option<String>,
    /// 检测端口（配置后以端口可连通判定运行，进程匹配仅用于假死时结束进程）
    pub detect_port: Option<u16>,
    /// 端口检测的主机，缺省 DEFAULT_DETECT_HOST
    pub detect_host: Option<String>,
}

impl EffectiveProgram {
    /// 端口检测目标（主机, 端口）；未配置端口时为 None
    pub fn detect_addr(&self) -> Option<(&str, u16)> {
        self.detect_port
            .map(|port| (self.detect_host.as_deref().unwrap_or(DEFAULT_DETECT_HOST), port))
    }
}

fn default_check_interval() -> u64 {
    DEFAULT_CHECK_INTERVAL_SECS
}

fn default_max_restarts() -> u32 {
    DEFAULT_MAX_RESTARTS_PER_HOUR
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取配置文件失败 {}：{e}", path.display()))?;
        let config: Config =
            toml::from_str(&text).map_err(|e| format!("解析配置文件失败：{e}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.programs.is_empty() {
            return Err("配置中未定义任何 [[programs]] 条目".into());
        }
        if self.check_interval_secs < 1 {
            return Err("check_interval_secs 必须 >= 1".into());
        }
        let mut names: HashSet<&str> = HashSet::new();
        for p in &self.programs {
            if p.name.trim().is_empty() {
                return Err("programs 中存在空的 name".into());
            }
            if !names.insert(p.name.as_str()) {
                return Err(format!("programs 中 name 重复：{}", p.name));
            }
            if p.path.as_os_str().is_empty() {
                return Err(format!("[{}] path 不能为空", p.name));
            }
            if let Some(d) = &p.detect {
                match d.port {
                    Some(0) => return Err(format!("[{}] detect.port 必须在 1~65535 之间", p.name)),
                    Some(_) => {
                        if let Some(h) = &d.host {
                            if h.trim().is_empty() {
                                return Err(format!("[{}] detect.host 不能为空字符串", p.name));
                            }
                            // 形如 IP 的主机（仅数字和点）必须是合法 IPv4：
                            // 笔误（如 10.162.161.190 写成 10162.161.190）会导致
                            // 端口检测永远失败，状态灯常红且触发假死误杀
                            let looks_like_ip =
                                h.chars().all(|c| c.is_ascii_digit() || c == '.');
                            if looks_like_ip && h.parse::<std::net::Ipv4Addr>().is_err() {
                                return Err(format!(
                                    "[{}] detect.host 不是合法的 IPv4 地址：{h}",
                                    p.name
                                ));
                            }
                        }
                    }
                    None => {
                        if d.host.is_some() {
                            return Err(format!(
                                "[{}] detect.host 需与 detect.port 一起配置",
                                p.name
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 校验并序列化写入配置文件（TOML 美化输出）。
    ///
    /// 注意：Windows 上 `rename` 不能覆盖已存在的目标文件，因此先删除再改名；
    /// 配置写入本身不是高频操作，可接受短暂的非原子窗口。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let text = toml::to_string_pretty(self).map_err(|e| format!("序列化配置失败：{e}"))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| format!("写入临时文件失败 {}：{e}", tmp.display()))?;
        if path.exists() {
            std::fs::remove_file(path)
                .map_err(|e| format!("删除旧配置文件失败 {}：{e}", path.display()))?;
        }
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("替换配置文件失败 {}：{e}", path.display()))?;
        Ok(())
    }

    /// 校验通过后生成带检测规则的条目列表
    pub fn effective_programs(&self) -> Vec<EffectiveProgram> {
        self.programs
            .iter()
            .map(|p| {
                let (detect_image, detect_cmdline) = p.effective_detect();
                EffectiveProgram {
                    name: p.name.clone(),
                    path: p.path.clone(),
                    args: p.args.clone(),
                    workdir: p.workdir.clone(),
                    java: p.java.clone(),
                    max_restarts_per_hour: p.max_restarts_per_hour,
                    start_delay_secs: p.start_delay_secs,
                    skip: p.skip,
                    detect_image,
                    detect_cmdline,
                    detect_port: p.detect.as_ref().and_then(|d| d.port),
                    detect_host: p.detect.as_ref().and_then(|d| d.host.clone()),
                }
            })
            .collect()
    }
}

impl Program {
    /// 计算生效的检测规则（小写）：（镜像名, 命令行子串）。
    ///
    /// `detect` 字段用于覆盖默认推导，未填写的部分保留默认。
    pub fn effective_detect(&self) -> (String, Option<String>) {
        let ext = self
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let (def_image, def_cmdline) = default_detect(&self.path, &ext);
        let detect = self.detect.clone().unwrap_or_default();
        let image = detect.image.unwrap_or(def_image).to_ascii_lowercase();
        let cmdline = detect
            .cmdline_contains
            .or(def_cmdline)
            .map(|s| s.to_ascii_lowercase());
        (image, cmdline)
    }

    /// 生成带生效检测规则的条目（供手动启停等单个程序操作使用）
    pub fn to_effective(&self) -> EffectiveProgram {
        let (detect_image, detect_cmdline) = self.effective_detect();
        EffectiveProgram {
            name: self.name.clone(),
            path: self.path.clone(),
            args: self.args.clone(),
            workdir: self.workdir.clone(),
            java: self.java.clone(),
            max_restarts_per_hour: self.max_restarts_per_hour,
            start_delay_secs: self.start_delay_secs,
            skip: self.skip,
            detect_image,
            detect_cmdline,
            detect_port: self.detect.as_ref().and_then(|d| d.port),
            detect_host: self.detect.as_ref().and_then(|d| d.host.clone()),
        }
    }
}

impl EffectiveProgram {
    /// 供 `validate` 子命令输出的一行可读描述
    pub fn describe(&self) -> String {
        let launch: String = match self.extension().as_str() {
            "exe" => "直接启动".to_string(),
            "bat" | "cmd" => "cmd.exe /c（隐藏窗口）".to_string(),
            "jar" => match &self.java {
                Some(j) => format!("{} -jar", j.display()),
                None => "java.exe -jar（JAVA_HOME/PATH 解析）".to_string(),
            },
            "ps1" => "powershell -File（隐藏窗口）".to_string(),
            _ => "直接启动".to_string(),
        };
        let mut proc_rule = format!("镜像名={}", self.detect_image);
        if let Some(c) = &self.detect_cmdline {
            proc_rule.push_str(&format!(" 或 命令行包含 \"{c}\""));
        }
        let rule = match self.detect_addr() {
            Some((host, port)) => format!(
                "端口 {host}:{port} 可连通（以此为准；假死时结束匹配进程：{proc_rule}）"
            ),
            None => proc_rule,
        };
        let delay = if self.start_delay_secs > 0 {
            format!(" | 启动延迟: {} 秒", self.start_delay_secs)
        } else {
            String::new()
        };
        let skip = if self.skip {
            " | 已跳过（不监控、不自动拉起）"
        } else {
            ""
        };
        format!(
            "[{}] path={} | args={:?} | workdir={:?} | 启动方式: {launch} | 检测: {rule}{delay} | 重启上限: {}/小时{skip}",
            self.name,
            self.path.display(),
            self.args,
            self.workdir.as_deref().unwrap_or_else(|| Path::new("")),
            self.max_restarts_per_hour,
        )
    }

    pub fn extension(&self) -> String {
        self.path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default()
    }
}

/// 按扩展名推导默认检测规则：
/// - exe  → 按自身镜像名匹配
/// - bat/cmd → 进程为 cmd.exe，按命令行中包含脚本路径匹配
/// - jar  → 进程为 java.exe，按命令行中包含 jar 路径匹配
/// - ps1  → 进程为 powershell.exe，按命令行中包含脚本路径匹配
fn default_detect(path: &Path, ext: &str) -> (String, Option<String>) {
    let path_str = path.to_string_lossy().to_string();
    match ext {
        "bat" | "cmd" => ("cmd.exe".to_string(), Some(path_str)),
        "jar" => ("java.exe".to_string(), Some(path_str)),
        "ps1" => ("powershell.exe".to_string(), Some(path_str)),
        _ => {
            let image = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or(path_str);
            (image, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_via_toml() {
        let config = Config {
            check_interval_secs: 3,
            log_file: Some(PathBuf::from(r"C:\Watchdog\logs\watchdog.log")),
            programs: vec![
                Program {
                    name: "myapp".into(),
                    path: PathBuf::from(r"C:\apps\myapp.exe"),
                    args: vec!["--port".into(), "8080".into()],
                    workdir: Some(PathBuf::from(r"C:\apps")),
                    java: None,
                    detect: None,
                    max_restarts_per_hour: 5,
                    start_delay_secs: 15,
                    skip: false,
                    log_file: Some(PathBuf::from(r"C:\apps\myapp\logs\app.log")),
                },
                Program {
                    name: "svc".into(),
                    path: PathBuf::from(r"C:\apps\svc.jar"),
                    args: vec![],
                    workdir: None,
                    java: Some(PathBuf::from(r"C:\java\bin\java.exe")),
                    detect: Some(DetectConfig {
                        image: Some("java.exe".into()),
                        cmdline_contains: Some(r"c:\apps\svc.jar".into()),
                        port: Some(8080),
                        host: Some("127.0.0.1".into()),
                    }),
                    max_restarts_per_hour: 0,
                    start_delay_secs: 0,
                    skip: true,
                    log_file: None,
                },
            ],
        };

        let text = toml::to_string_pretty(&config).expect("序列化失败");
        let back: Config = toml::from_str(&text).expect("反序列化失败");
        back.validate().expect("校验失败");

        assert_eq!(back.check_interval_secs, 3);
        assert_eq!(back.programs.len(), 2);
        assert_eq!(back.programs[0].name, "myapp");
        assert_eq!(back.programs[0].args, vec!["--port", "8080"]);
        assert_eq!(back.programs[0].start_delay_secs, 15);
        assert_eq!(back.programs[1].start_delay_secs, 0);
        assert_eq!(
            back.programs[0].log_file.as_deref(),
            Some(Path::new(r"C:\apps\myapp\logs\app.log")),
            "程序日志文件配置应在 TOML 往返中保留"
        );
        assert!(back.programs[1].log_file.is_none(), "未配置时应保持 None");
        assert!(!back.programs[0].skip, "未标记跳过的程序应保持 skip = false");
        assert!(back.programs[1].skip, "skip = true 应在序列化往返中保留");
        assert_eq!(back.programs[1].java.as_deref(), Some(Path::new(r"C:\java\bin\java.exe")));
        let detect = back.programs[1].detect.as_ref().expect("detect 丢失");
        assert_eq!(detect.image.as_deref(), Some("java.exe"));
        assert_eq!(detect.cmdline_contains.as_deref(), Some(r"c:\apps\svc.jar"));
        assert_eq!(detect.port, Some(8080));
        assert_eq!(detect.host.as_deref(), Some("127.0.0.1"));

        // 端口检测条目：detect_port / detect_host 进入生效条目，detect_addr 给出默认主机
        let eff = back.effective_programs().remove(1);
        assert_eq!(eff.detect_port, Some(8080));
        assert_eq!(eff.detect_addr(), Some(("127.0.0.1", 8080)));

        let hostless = Program {
            name: "h".into(),
            path: PathBuf::from(r"C:\h\h.exe"),
            detect: Some(DetectConfig {
                port: Some(9000),
                ..Default::default()
            }),
            ..make_program()
        }
        .to_effective();
        assert_eq!(hostless.detect_addr(), Some(("127.0.0.1", 9000)));
    }

    #[test]
    fn detect_port_validation() {
        let mut config = Config {
            check_interval_secs: 5,
            log_file: None,
            programs: vec![make_program()],
        };
        config.validate().expect("基础配置应通过");

        // detect.port = 0 非法
        config.programs[0].detect = Some(DetectConfig {
            port: Some(0),
            ..Default::default()
        });
        assert!(config.validate().is_err());

        // host 未与 port 一起配置
        config.programs[0].detect = Some(DetectConfig {
            host: Some("127.0.0.1".into()),
            ..Default::default()
        });
        assert!(config.validate().is_err());

        // host 为空字符串
        config.programs[0].detect = Some(DetectConfig {
            port: Some(80),
            host: Some("  ".into()),
            ..Default::default()
        });
        assert!(config.validate().is_err());

        // 形如 IP 但不合法（笔误少写点）→ 校验失败
        config.programs[0].detect = Some(DetectConfig {
            port: Some(80),
            host: Some("10162.161.190".into()),
            ..Default::default()
        });
        assert!(config.validate().is_err(), "非法 IPv4 应被拒绝");

        // 合法 IPv4 与普通主机名均放行
        config.programs[0].detect = Some(DetectConfig {
            port: Some(80),
            host: Some("10.162.161.190".into()),
            ..Default::default()
        });
        config.validate().expect("合法 IPv4 应通过");
        config.programs[0].detect = Some(DetectConfig {
            port: Some(80),
            host: Some("myhost".into()),
            ..Default::default()
        });
        config.validate().expect("普通主机名应通过");

        // 合法组合
        config.programs[0].detect = Some(DetectConfig {
            port: Some(80),
            host: Some("192.168.1.10".into()),
            ..Default::default()
        });
        config.validate().expect("合法端口配置应通过");
    }

    #[test]
    fn skip_field_default_and_describe_marker() {
        // TOML 中未写 skip → 默认 false，describe 不带跳过标记
        let text = r#"
            [[programs]]
            name = "a"
            path = "C:\\a\\a.exe"
        "#;
        let config: Config = toml::from_str(text).expect("解析失败");
        assert!(!config.programs[0].skip);
        let eff = config.effective_programs().remove(0);
        assert!(!eff.skip);
        assert!(!eff.describe().contains("已跳过"));

        // skip = true → 进入生效条目，describe 带跳过标记，序列化时保留
        let text = r#"
            [[programs]]
            name = "a"
            path = "C:\\a\\a.exe"
            skip = true
        "#;
        let config: Config = toml::from_str(text).expect("解析失败");
        assert!(config.programs[0].skip);
        let eff = config.effective_programs().remove(0);
        assert!(eff.skip);
        assert!(eff.describe().contains("已跳过"));
        assert!(config.programs[0].to_effective().skip);
        let saved = toml::to_string_pretty(&config).expect("序列化失败");
        assert!(saved.contains("skip = true"), "skip = true 应写入配置文件");
    }

    /// 构造一个最小合法的 Program（测试辅助）
    fn make_program() -> Program {
        Program {
            name: "t".into(),
            path: PathBuf::from(r"C:\t\t.exe"),
            args: vec![],
            workdir: None,
            java: None,
            detect: None,
            max_restarts_per_hour: 10,
            start_delay_secs: 0,
            skip: false,
            log_file: None,
        }
    }

    #[test]
    fn save_and_load_file() {
        let dir = std::env::temp_dir().join("watchdog-core-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.toml");

        let config = Config {
            check_interval_secs: 7,
            log_file: None,
            programs: vec![Program {
                name: "t".into(),
                path: PathBuf::from(r"C:\t\t.exe"),
                args: vec![],
                workdir: None,
                java: None,
                detect: None,
                max_restarts_per_hour: 10,
                start_delay_secs: 0,
                skip: false,
                log_file: None,
            }],
        };

        config.save(&path).expect("保存失败");
        config.save(&path).expect("覆盖保存失败"); // 已存在文件时的覆盖路径
        let back = Config::load(&path).expect("加载失败");
        assert_eq!(back.check_interval_secs, 7);
        assert_eq!(back.programs[0].name, "t");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
