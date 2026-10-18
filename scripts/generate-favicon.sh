#!/usr/bin/env bash
# 从一张现有图片（PNG / JPG / SVG）生成 AcmeCast 全套 favicon 资源。
#
# 用法：
#   scripts/generate-favicon.sh <图片路径> [输出目录]
#
# 输入：
#   - SVG：矢量源，直接光栅化为 512×512 高清中间图（质量最佳）
#   - PNG/JPG：居中裁剪为正方形后，最短边缩放至 512×512（JPG 无透明通道，
#     生成前请确认图片本身即为带圆角透明或方形满铺的 LOGO）
#
# 输出（写入 <输出目录>，默认 frontend/public）：
#   favicon.ico（16/32/48 三档）
#   favicon-16x16.png / favicon-32x32.png
#   apple-touch-icon.png（180×180）
#
# 依赖：ImageMagick（magick）。生成后请用 `file`/`identify` 抽检尺寸。

set -euo pipefail

usage() {
  echo "用法: $0 <图片路径 png|jpg|svg> [输出目录，默认 frontend/public]" >&2
  exit 1
}

[ $# -ge 1 ] || usage
SRC=$1
OUT=${2:-frontend/public}

command -v magick >/dev/null 2>&1 || { echo "错误：未安装 ImageMagick (magick)" >&2; exit 1; }
[ -f "$SRC" ] || { echo "错误：输入图片不存在：$SRC" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
MASTER="$WORK/master.png"

case "${SRC##*.}" in
  svg|SVG)
    # 矢量源：直接光栅化为 512×512（Q16 渲染 SVG 会得到 16-bit 色深，统一压到 8-bit）。
    magick -background none "$SRC" -strip -depth 8 -resize 512x512 "$MASTER"
    ;;
  png|PNG|jpg|jpeg|JPG|JPEG)
    # 位图源：居中裁正方形，再统一到 512×512。
    magick "$SRC" -strip -gravity center -crop '%[fx:min(w,h)]x%[fx:min(w,h)]+0+0' +repage \
      -resize 512x512 "$MASTER"
    ;;
  *)
    echo "错误：仅支持 png / jpg / svg，实际为：$SRC" >&2
    exit 1
    ;;
esac

mkdir -p "$OUT"

# 派生尺寸：PNG favicon 与 apple-touch-icon（-strip 去除时间戳等元数据块，
# 保证同一输入多次运行字节可复现）。
magick "$MASTER" -strip -resize 16x16 "$OUT/favicon-16x16.png"
magick "$MASTER" -strip -resize 32x32 "$OUT/favicon-32x32.png"
magick "$MASTER" -strip -resize 180x180 "$OUT/apple-touch-icon.png"

# 多尺寸 ICO：16/32/48 三档（按从小到大顺序编码目录项）。
magick "$MASTER" -strip -resize 48x48 "$WORK/ico48.png"
magick "$OUT/favicon-16x16.png" "$OUT/favicon-32x32.png" "$WORK/ico48.png" \
  -define ico:colors=0 "$OUT/favicon.ico"

echo "已生成（输出目录：$OUT）："
file "$OUT/favicon.ico" "$OUT/favicon-16x16.png" "$OUT/favicon-32x32.png" "$OUT/apple-touch-icon.png"
echo
echo "提示：site.webmanifest 引用路径不变，通常无需改动；"
echo "随后执行 pnpm -C frontend build 刷新构建产物。"
