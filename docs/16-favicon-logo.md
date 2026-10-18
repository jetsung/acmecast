# Favicon LOGO 生成提示词

> 用途：供专业 AI 绘图工具重新生成 AcmeCast 站点图标（favicon / PWA 图标）。
> 配色必须与网站品牌一致（登录页 `Brand` 组件、`theme-color: #4f63f2`）。
> 生成源码本文件位于仓库 `docs/16-favicon-logo.md`，配套生成脚本为
> `scripts/generate-favicon.sh`。

## 设计要素

- 外形：圆角方形 App 图标，**充满整张图片**（圆角方形容器即图片画布本身）
- 透明度：四个尖角处（圆角圆弧之外的区域）**必须为透明**（PNG alpha / SVG 无像素），不得用白色或任何底色填充
- 底色：135° 对角渐变，左上 `#4f63f2` → 右下 `#7b5bf5`，仅存在于圆角方形内部
- 主体：白色描边盾牌（不填充），盾牌内部一个白色对勾（✓）
- 风格：扁平极简，无阴影、无发光、无文字、无边框

## 中文提示词

极简扁平 App 图标设计：一个圆角正方形充满整张正方形画布，无任何外围留白或背景——画布即图标本身。背景为从左上角 #4f63f2（蓝紫色）到右下角 #7b5bf5（亮紫色）的 135 度对角渐变，色彩平滑无杂色。正方形的四个角为大圆角，圆角圆弧之外的区域必须保持完全透明（PNG alpha 通道透明，不是白色、不是棋盘格填充），如同标准 iOS/Android 应用图标的透明尖角。图标中央是一个白色线条盾牌轮廓（描边风格，不填充），盾牌内部有一个简洁的白色对勾（✓），线条粗细均匀、圆角收尾。整体风格现代、干净、扁平化，无阴影、无渐变光泽、无描边边框、无文字、无水印。用途：浏览器 favicon 与 PWA 应用图标，需在 16×16 至 512×512 尺寸下均清晰可辨。

## 英文提示词

Minimal flat app icon design: a single rounded square that fills the ENTIRE square canvas edge-to-edge — the canvas IS the icon, with no outer margin or background around it. Fill: smooth 135-degree diagonal gradient from #4f63f2 (top-left) to #7b5bf5 (bottom-right), no color banding. The square has large rounded corners; the four corner areas OUTSIDE the rounded arcs must be fully TRANSPARENT (PNG alpha channel — not white, not checkerboard), like standard iOS/Android app icon silhouettes. Centered on the square, a white outlined shield shape (stroke only, no fill) containing a clean white checkmark, with uniform stroke width and rounded line caps. Modern, clean, flat design — no shadows, no glossy effects, no borders, no text, no watermark. Intended for use as a browser favicon and PWA app icon; must remain legible from 16×16 up to 512×512.

## SVG 定制提示词（矢量输出专用）

供支持 SVG 输出的 AI 工具使用，直接产出可提交仓库的矢量源：

```text
Generate ONLY valid SVG code with these exact specifications:

1. Shape: A single rounded square that fills the ENTIRE 512×512 canvas edge-to-edge — the canvas IS the icon, with no outer margin, no extra background rect, no surrounding layer.
2. True Transparent Corners: The four corner areas OUTSIDE the rounded arcs must remain natively transparent in SVG (no background rect, not white, not checkerboard), matching standard iOS/Android app icon silhouettes.
3. Gradient Fill: Smooth 135-degree diagonal linearGradient (x1="0%", y1="0%", x2="100%", y2="100%") from #4f63f2 (top-left) to #7b5bf5 (bottom-right).
4. Center Graphic: Centered on the square, a white (#FFFFFF) outlined shield shape (stroke only, fill="none") containing a clean centered white checkmark (stroke only, fill="none").
5. Stroke & Style: Uniform bold stroke width (stroke-width="28"), rounded line caps (stroke-linecap="round"), and rounded line joins (stroke-linejoin="round"). Modern, clean, flat vector design — no shadows, no glossy effects, no borders, no text, no watermark. Must remain legible from 16×16 browser favicon up to 512×512 PWA app icon.

Output ONLY valid SVG code.
```

## 使用建议

1. 生成时选择 **1:1 正方形** 输出；优先选支持矢量（SVG）或透明 PNG 输出的工具（如 Recraft、Ideogram），方便直接派生多尺寸。
2. **交付前自检透明角**：用图像查看器或 `magick <file> -format "%[channels]" info:` 确认图片带 alpha 通道；将图放在深色背景上目检，四个尖角处应透出背景而非白块。SVG 输出则确认圆角矩形外无 `<rect>` 底层。
3. 拿到新图后，用仓库脚本一键重生成 `frontend/public/` 下全套 favicon：

   ```bash
   scripts/generate-favicon.sh <新图路径>   # 支持 png / jpg / svg
   pnpm -C frontend build                   # 刷新构建产物
   ```

4. 不要让工具添加文字（如 "AC" 字母）——当前规格（`console-favicon`）要求「不得引入额外文字或边框」；若确需文字，先同步更新 spec 再生成。
