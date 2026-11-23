# -*- coding: utf-8 -*-
"""品牌素材生成脚本：从 assets/logo/png 的 PNG 导出打包所需位图。

产物：
  installer/assets/sidebar.bmp   欢迎页/完成页侧边图 164x314（NSIS MUI_WELCOMEFINISHPAGE_BITMAP）
  installer/assets/header.bmp    向导页眉图 150x57（NSIS MUI_HEADERIMAGE_BITMAP）
  crates/watchdog-gui/assets/icon.rgba
                                 GUI 窗口图标原始 RGBA 像素（256x256，include_bytes 用，
                                 避免 GUI 引入 image 解码依赖）

品牌源文件见 assets/logo/（README 含设计理念与色板）。
重新生成：python installer/tools/gen_brand_assets.py（需 Pillow）
"""

from PIL import Image
import os

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LOGO = os.path.join(ROOT, "assets", "logo", "png")
OUT = os.path.join(ROOT, "installer", "assets")
GUI_ASSETS = os.path.join(ROOT, "crates", "watchdog-gui", "assets")

badge = Image.open(os.path.join(LOGO, "watchdog-icon-256.png")).convert("RGBA")
horiz = Image.open(os.path.join(LOGO, "watchdog-horizontal-on-light.png")).convert("RGBA")
# 纯字标裁剪：横版 PNG 左侧是徽章（x<568），从空隙后取字标部分（含心跳尖峰）
wordmark = horiz.crop((620, 60, 1640, 584))


def vgrad_rgb(w, h, top, bottom):
    """竖直渐变纯 RGB 图。"""
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / (h - 1)
        c = tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = c
    return img


def paste_rgba(base, im, xy):
    base.paste(im, xy, im)


# ---- 欢迎页侧边图 164x314：浅蓝渐变底 + 徽章 + 纯字标 ----
W, H = 164, 314
side = vgrad_rgb(W, H, (235, 242, 250), (253, 254, 255))
b = badge.resize((120, 120), Image.LANCZOS)
paste_rgba(side, b, ((W - 120) // 2, 48))
wm = wordmark.resize((120, round(120 * wordmark.height / wordmark.width)), Image.LANCZOS)
paste_rgba(side, wm, ((W - wm.width) // 2, 216))

# ---- 页眉图 150x57：白底 + 小徽章 + 纯字标 ----
wm2 = wordmark.resize((74, round(74 * wordmark.height / wordmark.width)), Image.LANCZOS)
head = Image.new("RGB", (150, 57), (255, 255, 255))
b2 = badge.resize((46, 46), Image.LANCZOS)
paste_rgba(head, b2, (6, 5))
paste_rgba(head, wm2, (60, (57 - wm2.height) // 2))

os.makedirs(OUT, exist_ok=True)
side.save(os.path.join(OUT, "sidebar.bmp"))
head.save(os.path.join(OUT, "header.bmp"))

# ---- GUI 窗口图标原始 RGBA（egui IconData 直接消费，尺寸须为 4 的倍数）----
assert badge.size == (256, 256)
os.makedirs(GUI_ASSETS, exist_ok=True)
with open(os.path.join(GUI_ASSETS, "icon.rgba"), "wb") as f:
    f.write(badge.tobytes())

print("sidebar.bmp", side.size, "header.bmp", head.size, "icon.rgba", badge.size)
