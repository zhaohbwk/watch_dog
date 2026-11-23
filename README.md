# 应用看门狗（Application Watchdog）

Rust 编写的 Windows 应用看门狗：以 **Windows 服务**方式运行，定期检测配置的程序是否在运行，未运行时自动拉起。支持 `exe` / `bat` / `cmd` / `jar` / `ps1`，适配 **Windows Server 2022**，并提供图形配置工具 `watchdog-gui.exe`。

## 功能

- **多程序监控**：一个配置文件管理多个被监控程序
- **一键全部启动 / 全部停止**：GUI 中一键批量启动所有未运行的程序、一键批量停止所有程序；「全部停止」时自动**暂停自动拉起**（暂停标记文件），避免程序被服务在下个检测周期重新拉起，「全部启动」时恢复拉起
- **开机自启**：注册为 Windows 服务（`start= auto`），服务器重启后无需登录即自动运行
- **自动拉起**：检测到程序未运行（进程退出/被误杀/从未启动）时自动启动
- **启动顺序控制**：每个程序可配置启动延迟（`start_delay_secs`），实现先启动服务端、再按间隔启动依赖它的模型/客户端
- **按程序停用**：`skip = true` 可临时跳过某个程序的监控与自动拉起，配置保留，随时恢复
- **看门狗自身保活**：通过失败恢复策略配置，看门狗崩溃后由服务管理器自动重启
- **崩溃循环保护**：每个程序可配置一小时内最大自动启动次数
- **灵活匹配**：默认按进程镜像名检测；jar/bat 等按命令行匹配（区分同一机器上的多个 java/cmd 进程）
- **端口检测（假死恢复）**：可为程序配置检测端口，以"端口可连通"判定运行；进程在但端口持续不通时判定假死，自动结束并重新拉起
- **图形配置工具**：`watchdog-gui.exe` 可视化增删改监控程序、一键管理服务、查看日志
- **文件日志**：带时间戳的日志，超过 5MB 自动轮转
- **程序输出日志**：每个被监控程序的控制台输出（stdout/stderr）记录到与看门狗日志同目录的 `<名称>.log`，GUI 可按程序查看；程序启动失败、抛异常时可直接定位原因
- **程序日志文件实时查看**：每个程序可配置其自身写出的日志文件路径（`programs[].log_file`，如 Java 应用的 `logs\app.log`），GUI 日志窗口可切换到「（日志文件）」条目实时查看，便于在不打开控制台输出的情况下跟踪业务日志

## 目录结构

```
watchdog/                         # Cargo workspace
├── Cargo.toml
├── watchdog.example.toml         # 配置样例（复制为 watchdog.toml 使用）
├── assets/                       # 品牌 Logo（设计理念与色板见 assets/logo/README.md）
├── installer/                    # Windows 安装包（NSIS）
│   ├── watchdog.nsi              # 安装程序脚本（配置覆盖提示 / 服务处理 / 卸载）
│   ├── build_installer.ps1       # 一键打包（构建 + 自动下载 NSIS + 生成 setup exe）
│   ├── assets\                   # 打包位图（由 installer\tools\gen_brand_assets.py 生成）
│   └── dist\                     # 打包产物 watchdog-setup-<版本>.exe
├── scripts/
│   ├── install_service.ps1       # 命令行方式安装服务（管理员运行）
│   └── uninstall_service.ps1     # 命令行方式卸载服务（管理员运行）
└── crates/
    ├── watchdog-core/            # 共享核心：配置解析/监控循环/拉起/日志/服务集成
    ├── watchdog/                 # 服务端 CLI：watchdog.exe（run / foreground / validate）
    └── watchdog-gui/             # 图形配置工具：watchdog-gui.exe
```

## 构建

在 Windows 机器（或 Windows Server 2022 本机）上：

1. 安装 [Rust](https://rustup.rs/)（默认 MSVC 工具链），并安装 [VS Build Tools](https://visualstudio.microsoft.com/zh-hans/visual-cpp-build-tools/)（勾选「使用 C++ 的桌面开发」）
2. 构建发布版：

   ```powershell
   cd watchdog
   cargo build --release
   ```

3. 产物：
   - `target\release\watchdog.exe` — 服务端（无界面，注册为服务后后台运行）
   - `target\release\watchdog-gui.exe` — 图形配置工具（双击运行）

### 版本号

格式为「年.月日.时分」，如 `2026.1123.1114`，即**本次编译时刻的本地时间**，由构建脚本在编译期自动生成（无需手动维护）。查看方式：

- `watchdog.exe version`（或 `--version` / `-V`）
- `watchdog.exe validate` 输出中的「版本号」行
- `watchdog-gui.exe` 窗口标题栏

> 版本号随实际构建自动更新，可用于确认部署的 exe 是哪次构建（改动代码后重新 `cargo build --release`，版本号即变为新时间）。

> 提示：`cargo-xwin` 可在非 Windows 机器上交叉编译 Windows 目标（`cargo xwin build --release --target x86_64-pc-windows-msvc`），但正式上线前建议在 Windows 上构建并测试。

## Windows 安装包

一键打包为 Windows 安装程序（NSIS 中文向导）：

```powershell
cd watchdog
powershell -ExecutionPolicy Bypass -File installer\build_installer.ps1
```

脚本自动完成：刷新版本号（触发重编译，版本号 = 本次编译时间）→ `cargo build --release` → 读取版本号 → 首次运行自动下载 NSIS 便携版到 `installer\nsis\`（解压即用，不改动系统）→ 生成 `installer\dist\watchdog-setup-<版本>.exe`。

> 注意：若看门狗服务正从 `target\release\watchdog.exe` 运行（开发机直接在构建目录装服务的情况），文件被占用会导致构建失败——先以管理员执行 `sc stop watchdog`，打包完成后再启动。加 `-NoVersionBump` 参数可跳过版本号刷新（保留上次编译时间戳）。

### 安装器行为

- **配置覆盖提示**：安装 / 升级时若安装目录已有 `watchdog.toml`，会弹窗询问是否覆盖——「是」覆盖为新版默认配置（原配置自动备份为 `watchdog.toml.bak`），「否」保留现有配置；无论选择哪种，`watchdog.example.toml` 都会更新为新版样例，新字段可对照手工合并。全新安装（无配置文件）自动生成默认配置。
- **升级处理**：安装前自动停止运行中的 `watchdog` 服务并在安装完成后恢复启动；自动关闭运行中的 `watchdog-gui.exe`（先温和关闭，超时后强制结束）。
- **附加任务**（向导中可勾选，默认勾选）：创建桌面快捷方式；安装并启动 Windows 服务（开机自启 + 崩溃自动重启，等效于 `scripts\install_service.ps1`；服务已注册时仅启动）。
- **卸载**：停止并删除 Windows 服务，删除程序文件与快捷方式；**保留** `watchdog.toml`、`watchdog.toml.bak`、`logs\`、`*.pause` 等运行数据，可在控制面板「应用」中卸载。
- **静默安装**（无人值守部署）：`watchdog-setup-xxx.exe /S /D=C:\Watchdog`（`/D` 指定目录，必须是最后一个参数）。静默模式默认**保留**已有配置；可选参数：`/overwriteconfig` 强制覆盖已有配置（旧配置同样备份为 `.bak`）、`/noservice` 跳过服务注册、`/nodesktop` 不创建桌面快捷方式。

### 重新打包说明

- 安装脚本在 `installer\watchdog.nsi`（UTF-8 with BOM 编码，makensis 依赖 BOM 识别 UTF-8，脚本会自动转换）。
- 测试构建：`installer\nsis\makensis.exe -DTESTBUILD watchdog.nsi` 生成免管理员要求的测试包（`watchdog-setup-TEST-*.exe`），注册表与服务操作被跳过，便于无人值守冒烟测试。

### 品牌与图标

Logo 为「戴夜视仪的守夜犬」（定稿与设计理念见 `assets/logo/README.md`），全链路应用：

| 位置 | 实现 |
| --- | --- |
| 安装程序 / 卸载程序图标 | NSIS `MUI_ICON` / `MUI_UNICON` → `assets\logo\watchdog.ico` |
| 安装向导页眉图、欢迎/完成页侧边图 | `installer\assets\header.bmp`（150x57）、`sidebar.bmp`（164x314） |
| `watchdog.exe` / `watchdog-gui.exe` 文件图标 | 两个 crate 的 `build.rs` 用 `winresource` 嵌入 `watchdog.ico`（需 MSVC 工具链） |
| `watchdog-gui.exe` 窗口标题栏 / 任务栏 | egui `with_icon`，原始像素由 `installer\tools\gen_brand_assets.py` 从 256px PNG 预导出为 `crates\watchdog-gui\assets\icon.rgba` |

修改 Logo 后重新出图：改 `assets\logo\` 下 SVG → 按其 README 导出 PNG → 运行 `python installer\tools\gen_brand_assets.py` 重新生成位图与 rgba，再执行 `build_installer.ps1` 打包。

## 部署

建议目录规划：

```
C:\Watchdog\
├── watchdog.exe
├── watchdog-gui.exe   # 图形配置工具
├── watchdog.toml      # 由 watchdog.example.toml 复制修改
└── logs\watchdog.log  # 运行后自动生成
```

**方式一：使用图形配置工具（推荐）**

1. 复制 `watchdog.exe`、`watchdog-gui.exe` 到 `C:\Watchdog\`
2. 双击运行 `watchdog-gui.exe`（服务管理需管理员权限，界面会有提示，可一键提权重启）
3. 在界面中添加要监视的程序（目录 + 文件名 + 启动参数等），保存配置
4. 点击「安装」一键注册并启动服务（等效于安装脚本，含开机自启与失败恢复策略）
5. 打开「日志」开关查看看门狗运行日志

**方式二：命令行**

1. 复制 `watchdog.exe` 到 `C:\Watchdog\`
2. 复制 `watchdog.example.toml` 为 `C:\Watchdog\watchdog.toml`，按需修改
3. **校验配置**：

   ```powershell
   C:\Watchdog\watchdog.exe validate
   ```

4. **前台试运行**（验证能正确检测和拉起，Ctrl+C 退出，日志同步输出到控制台）：

   ```powershell
   C:\Watchdog\watchdog.exe foreground
   ```

   此时可手动结束某个被监控程序，观察其是否在下一个检测周期内被自动拉起。

5. **安装服务**（管理员 PowerShell）：

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\scripts\install_service.ps1
   # 或指定部署路径：
   # powershell -ExecutionPolicy Bypass -File .\scripts\install_service.ps1 -ExePath C:\Watchdog\watchdog.exe -ConfigPath C:\Watchdog\watchdog.toml
   ```

6. 验证：

   ```powershell
   sc.exe query watchdog
   Get-Content C:\Watchdog\logs\watchdog.log -Tail 20
   ```

## 进程与端口查询

无需配置文件与服务即可使用的排查命令（功能类似 netstat 的按需查询，IPv4 / IPv6 都支持）：

```powershell
# 根据端口查询监听进程的 PID 与进程名（可核对检测端口是否被正确的程序占用）
C:\Watchdog\watchdog.exe port 8080

# 根据进程名查询其监听的所有端口号（常用于确认 java 等服务实际监听了哪些端口）
C:\Watchdog\watchdog.exe proc java
```

说明：

- `proc` 的进程名**忽略大小写与 .exe 后缀**：`java`、`java.exe`、`JAVA.EXE` 等价
- 同一进程名有多个实例时按 PID 逐个列出各自的监听端口；结果为空或未找到时以退出码 1 结束，便于脚本判断
- 未以管理员运行时查询同样准确（监听表无需特权；这与读取进程命令行不同）

## 图形配置工具（watchdog-gui.exe）

| 区域 | 功能 |
| --- | --- |
| 顶部工具栏 | 打开/保存/另存为配置；服务状态显示；安装/启动/停止/重启/卸载服务；日志开关 |
| 左侧列表 | 被监控程序列表（新增、选中编辑、右键删除/**启动/停止/重启**）；**每项前置运行状态灯：绿=运行中，红=未运行**（每 3 秒刷新，检测规则与服务端一致）；列表上方提供 **▶ 全部启动 / ■ 全部停止** 一键批量操作，暂停时显示「⏸ 自动拉起已暂停」提示 |
| 右侧表单 | 名称、目录 + 文件名（可浏览选择）、启动参数（每行一个）、工作目录、日志文件（可选，程序自己写出的日志，供日志窗口实时查看）、java 路径（jar）、高级检测规则（镜像名 / 命令行 / **端口检测**）、重启上限、启动延迟；表单下方提供该程序的 **▶ 启动 / ■ 停止 / ↻ 重启 / 📄 日志** 手动操作 |
| 底部 | 全局设置：检测间隔、日志文件路径 |

说明：

- **类型自动推断**：由所选文件扩展名决定（exe / bat / cmd / jar / ps1），选择文件后自动识别
- **运行状态灯**：程序列表每项前的圆点实时反映运行状态（绿=运行中，红=未运行）；注意未以管理员运行时，其它用户/服务进程的命令行可能读不到，jar/bat 类程序可能误报为"未运行"（提权后即准确）
- **单程序手动启停**：列表右键菜单或编辑表单下方按钮可单独启动/停止/重启某个程序。停止时**配置了端口检测的程序优先按监听端口的进程 PID 精确终止**——只杀真正监听该端口的进程，不会误杀镜像名/命令行相同的其它程序（端口上无监听进程时才回退按检测规则匹配）；未配置端口的 exe 仍按镜像名匹配，会连同同名实例一起终止。若看门狗服务正在运行，手动停止的程序会在下个检测周期被自动拉起，需长期停用请先停服务或从配置中删除该条目
- **一键全部启动 / 全部停止**：程序列表上方的批量按钮，详见下方「一键全部启停与暂停自动拉起」
- **检测规则**：默认按类型自动推导（如 jar → 进程 `java.exe` + 命令行含 jar 路径），可在「高级检测规则」中覆盖，界面会实时显示当前生效规则；启用「端口检测」后以端口可连通为准（状态灯同步切换为按端口显示）
- **保存**：点击「保存」时自动校验（名称唯一/非空、路径必填等），出错会在状态栏提示
- **服务管理**：需管理员权限；「安装」前会自动保存配置，并默认选用同目录的 `watchdog.exe`
- **日志**：日志窗口可下拉切换查看「看门狗日志」、各被监控程序的控制台输出「（控制台）」，以及配置了「日志文件」的程序的业务日志「（日志文件）」（读取末尾约 256KB，默认每 2 秒自动刷新，可勾选关闭或手动「立即刷新」）；编辑表单下方「📄 日志」按钮直接打开当前程序的日志——配置了「日志文件」时打开业务日志，否则打开控制台输出
- **程序输出日志**：与看门狗日志同目录的 `logs\<名称>.log`，追加模式，每次（自动或手动）拉起前写入一行 `===== [时间] 看门狗启动：… =====` 分隔标记；超过 5MB 在下次拉起前轮转为 `.log.1`
- 注意：GUI 保存会整体重写 TOML 文件，**原文件中的注释不会被保留**（字段说明以 `watchdog.example.toml` 为准）

### 一键全部启停与暂停自动拉起

程序列表上方提供 **▶ 全部启动** 与 **■ 全部停止** 两个批量按钮（已停用 `skip = true` 的程序不受影响）：

- **▶ 全部启动**：只启动当前未运行的程序（不会拉起重复实例），同时移除暂停标记、恢复服务的自动拉起
- **■ 全部停止**：按配置**逆序**终止所有程序（后启动的先停，符合依赖关闭顺序）；配置了端口检测的程序按监听端口的 PID 精确终止，不会误杀同名其它程序
- **暂停自动拉起**：看门狗服务运行中时，「全部停止」会先写入**暂停标记文件**（与配置文件同目录同名、扩展名为 `.pause`，如 `C:\Watchdog\watchdog.pause`），服务每轮检测循环检查该文件——存在时**暂停自动拉起与假死恢复**（已运行的程序保持运行，进入/恢复各记一条日志），避免刚停止的程序被服务在下个检测周期重新拉起；界面同步显示「⏸ 自动拉起已暂停」
- **恢复自动拉起**：点「▶ 全部启动」，或服务的「启动 / 重启 / 安装」按钮（显式恢复监控的操作会顺带移除暂停标记）
- 暂停期间单个程序仍可手动启动/停止；看门狗日志中会有「暂停自动拉起 / 恢复自动拉起」记录

## 配置说明

配置文件为 TOML，默认位于 `watchdog.exe` 同目录的 `watchdog.toml`，可用 `--config` 参数覆盖。

| 字段 | 说明 |
| --- | --- |
| `check_interval_secs` | 检测间隔（秒），默认 5，最小 1 |
| `log_file` | 日志路径，默认 `exe 同目录\logs\watchdog.log` |
| `programs[].name` | 唯一名称（用于日志与状态跟踪） |
| `programs[].path` | 程序路径（exe / bat / cmd / jar / ps1） |
| `programs[].args` | 启动参数字符串数组 |
| `programs[].workdir` | 工作目录；**缺省为程序所在目录**（避免相对路径找不到资源导致启动失败） |
| `programs[].java` | jar 程序指定的 `java.exe` 路径；缺省依次取 `JAVA_HOME\bin\java.exe`、PATH 中的 `java.exe` |
| `programs[].detect.image` | 覆盖默认的进程镜像名匹配（小写） |
| `programs[].detect.cmdline_contains` | 按命令行子串匹配（小写，大小写不敏感） |
| `programs[].detect.port` | 检测端口（1~65535）：配置后以端口可连通判定运行，见下方「端口检测」说明 |
| `programs[].detect.host` | 端口检测的主机（缺省 `127.0.0.1`），需与 `detect.port` 一起配置；IP 形式（仅数字和点）必须是合法 IPv4（校验拒绝笔误，如 `10162.161.190`），否则端口检测永远失败并触发假死误杀 |
| `programs[].max_restarts_per_hour` | 一小时内最多自动启动次数（默认 10，0 为不限制） |
| `programs[].start_delay_secs` | 启动延迟秒数（默认 0）：看门狗启动后等待 N 秒才首次拉起该程序，用于控制启动顺序（如先服务端后模型）；只约束首次拉起，之后的自动重启不受影响 |
| `programs[].skip` | 跳过监控（默认 `false`）：`true` 时看门狗不监控、不自动拉起该程序，配置保留，改回 `false` 或删除该字段即可恢复；GUI 中的手动启动/停止不受影响 |
| `programs[].log_file` | 程序自己写出的日志文件路径（可选，每程序一个）：仅供 GUI 日志窗口实时查看（如 Java 应用的 `logs\app.log`），看门狗不读取、不轮转该文件；缺省时日志窗口仅显示控制台输出。对应 GUI 中每个程序的「日志文件」输入框 |

### 检测与启动规则

| 文件类型 | 启动方式 | 默认检测规则 |
| --- | --- | --- |
| `.exe` | 直接启动 | 进程镜像名 = 文件名（如 `myapp.exe`） |
| `.bat` / `.cmd` | `cmd.exe /c`（隐藏窗口） | 进程为 `cmd.exe`，命令行包含脚本完整路径 |
| `.jar` | `java -jar`（隐藏窗口） | 进程为 `java.exe`，命令行包含 jar 完整路径 |
| `.ps1` | `powershell -NoProfile -ExecutionPolicy Bypass -File`（隐藏窗口） | 进程为 `powershell.exe`，命令行包含脚本完整路径 |

`detect` 字段用于覆盖默认规则（如 exe 拉起后实际驻留进程名不同、或需更精确地区分多个同类进程），未填写的部分保留默认。

### 端口检测（假死恢复）

配置 `detect.port` 后，**端口可连通是判定程序运行中的唯一标准**：

- 端口可连通 → 运行中；端口不通且无匹配进程 → 按常规流程自动拉起
- **假死恢复**：端口持续不通（宽限 60 秒，覆盖服务启动慢的情况）且进程仍在 → 判定假死，先按镜像名/命令行规则结束匹配进程，再于下个检测周期重新拉起
- `detect.host` 缺省检测 `127.0.0.1`；服务监听 `0.0.0.0` 时无需修改，仅监听某个具体网卡 IP 时才需改成对应地址
- **精确停止**：GUI 的停止 / 重启 / 全部停止，以及假死恢复时结束进程，都会优先通过监听端口查到进程 PID 再终止（IPv4 / IPv6 都支持），只杀真正监听该端口的进程，不会误杀同名的其它程序；端口上无监听进程时（如假死进程已不监听）才回退按镜像名/命令行规则结束
- GUI 状态灯与 `validate` 输出同样以端口为准

```toml
[[programs]]
name = "web-server"
path = "C:\\apps\\web\\server.exe"
detect = { port = 8080 }
```

> 注意：假死恢复按检测规则结束进程，请确保镜像名/命令行规则能匹配到实际服务进程；若程序并不监听所配置的端口（如纯计算型程序），不要启用端口检测，否则会被反复结束重启（受重启上限约束）。

> **同名 exe 提示**：多个目录下的程序若镜像名相同（如 `model_radar\Model.exe` 与 `model_rf\Model.exe`），按镜像名检测会互相误判为“运行中”。此时必须为每个程序设置 `detect.cmdline_contains` 为各自的完整路径（如 `C:\...\model_radar\Model.exe`），按命令行区分。

## 服务管理

```powershell
sc.exe query watchdog          # 查看状态
sc.exe stop watchdog           # 停止（已拉起的被监控程序保持运行）
sc.exe start watchdog          # 启动
powershell -ExecutionPolicy Bypass -File .\scripts\uninstall_service.ps1   # 卸载
```

- 看门狗停止/卸载时**不会**终止它此前拉起的被监控程序。
- 看门狗自身崩溃后由 SCM 自动重启（24 小时内最多 3 次，间隔 60 秒）。

## 注意事项

- **Session 0 限制**：服务以 SYSTEM 用户运行于 Session 0，它拉起的 GUI 程序界面不可见。本看门狗适合后台服务类程序（无界面 exe、jar 服务、脚本）；需要交互界面的程序请改用其它方案（如登录后的计划任务）。
- **bat 一次性脚本**：脚本退出后看门狗会再次拉起它，直到达到 `max_restarts_per_hour` 上限。周期性任务建议使用任务计划程序。
- **权限**：以 SYSTEM 运行，读取其它用户进程的命令行没有权限问题。
- **网络盘/权限**：被监控程序路径需对 SYSTEM 账户可访问（默认本地路径无问题）。

## 常见问题

- **服务启动后立即停止**：先运行 `watchdog.exe validate` 检查配置；再看 `C:\Watchdog\logs\watchdog.log` 中的错误。
- **程序启动失败 / 启动后异常退出**：查看 `logs\<名称>.log`（GUI 选中程序点「📄 日志」），其中记录了每次启动分隔标记与程序自身的控制台输出（含报错堆栈）。
- **程序被重复拉起**：确认 `detect` 规则与实际进程匹配（用任务管理器查看进程名/命令行）；java 类程序如果命令行读取失败，看门狗会认为其未运行。
- **想临时全部停掉程序做维护，但程序又被拉起来了**：单个「■ 停止」后服务会在下个检测周期重新拉起（属正常行为）。请使用程序列表上方的「■ 全部停止」——它会自动写入暂停标记、暂停服务的自动拉起；维护完点「▶ 全部启动」恢复。界面显示「⏸ 自动拉起已暂停」即表示暂停生效。
- **程序被反复结束又重启**：检查是否配置了 `detect.port` 而程序实际并不监听该端口（或仅监听在 `detect.host` 访问不到的地址）——端口检测为准时会被判定假死并反复重启。
- **不想监控某个程序但又不想删掉配置**：在 GUI 中勾选该程序的「跳过监控」（或配置里写 `skip = true`），保存后重启服务即可；恢复时取消勾选再重启服务。
- **修改配置后不生效**：修改 `watchdog.toml` 后需重启服务：`sc.exe stop watchdog && sc.exe start watchdog`。

## 开源许可

本项目基于 [MIT License](LICENSE) 开源发布。
