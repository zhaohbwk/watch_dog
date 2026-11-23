# watch_dog 品牌 Logo

> 定稿日期：2026-11-23。矢量源文件为本目录下的 SVG，其余格式均由其导出。

## 设计理念

**「戴夜视仪的守夜犬」**——一只低多边形（几何切面）风格的犬首，凝视着被守护的程序：

| 元素 | 含义 |
| --- | --- |
| 几何切面犬首 | 看门狗本体；硬朗切面呼应工程工具的可靠感 |
| 深色面罩 + 发光双眼 | 「夜视护目镜」：全天候值守、时刻监视 |
| 绿色眼睛 | 呼应 GUI 程序列表的状态灯（**绿 = 运行中**） |
| 心跳脉冲线 | 被守护程序的生命体征——看门狗的存在意义就是让这条线一直跳下去 |

横版字标把 `watch_dog` 中的下划线 `_` 替换成了一记**心电图尖峰**：分隔符本身也是语义（“watch 的心跳、dog 的脉搏”）。

## 文件清单

| 文件 | 说明 |
| --- | --- |
| `watchdog-icon.svg` | 主图标：深色徽章（默认，exe/安装包/桌面图标） |
| `watchdog-icon-light.svg` | 主图标：浅色徽章（浅色背景的文档/网页） |
| `watchdog-mark-on-dark.svg` | 无底徽标，浅色狗头，透明底（深色背景用） |
| `watchdog-mark-on-light.svg` | 无底徽标，深色狗头，透明底（浅色背景用） |
| `watchdog-horizontal-on-light.svg` | 横版组合「徽章 + watch_♥dog 字标」，浅色背景用 |
| `watchdog-horizontal-on-dark.svg` | 横版组合，深色背景用 |
| `watchdog.ico` | Windows 图标，含 256/128/64/48/32/16 六档尺寸 |
| `png/` | 各变体的 PNG 导出（含 1024→16 全尺寸透明底） |

## 色板

| 用色 | 色值 | 用途 |
| --- | --- | --- |
| 深空蓝 | `#0A1428` → `#14263F`（渐变） | 徽章底 |
| 冰蓝白 | `#F4FBFF` → `#BBD9EE`（渐变） | 犬首（深底版） |
| 藏蓝 | `#33587F` → `#1E3D61`（渐变） | 犬首（浅底版） |
| 护目镜蓝 | `#2B4B72` → `#1D3A5C`（深底）/ `#7EA7CC` → `#5C87AE`（浅底） | 面罩 |
| 状态绿 | `#5DF08C` → `#1FBF62`（深底）/ `#43DC7F` → `#0EA352`（浅底）；线稿 `#2EE27A` / `#12AC5B` | 眼睛、脉冲线 |
| 墨蓝 | `#0E1E36` / `#10253F` | 鼻子、浅底字标文字 |
| 冰白 | `#EAF3FC` | 深底字标文字 |

字标字体：**Consolas Bold**（等宽，呼应开发者工具），SVG 内用 `textLength` 锁定字宽，即使字体回退 `watch` / `dog` 与中间的心跳尖峰也不会错位。

## 使用建议

- **exe / 安装包图标**：直接使用 `watchdog.ico`（NSIS 脚本 `Icon` 指令、GUI 窗口图标均可用）。
- **README / 文档**：优先横版 `watchdog-horizontal-on-*.svg`（GitHub 会按背景自动适配时选浅色版即可）。
- **最小使用尺寸**：徽章图标建议不小于 16px（16px 时眼睛与轮廓仍可辨，脉冲线退化为绿色底线，属预期）。
- **留白**：徽章四周建议保留 ≥ 8% 边距；勿拉伸变形、勿更换色板。

## 重新渲染 / 修改

矢量源为 SVG，改完可用 `_tmp/render.py`（Edge 无头截图）重新导出 PNG，再用 Pillow 缩放生成多尺寸与 .ico：

```bash
cd assets/logo
python _tmp/render.py watchdog-icon.svg preview.png 512          # 512px 预览（最后参数可传背景色）
python _tmp/render.py watchdog-horizontal-on-light.svg h.png 1640x640 "#FFFFFF"
```
