"""Render the three marks: standalone PNGs, plate lockups, and one contact
sheet with Cyrillic labels (DejaVu has the glyphs, so no font hunting)."""

import os
import re
import subprocess

OUT = "/home/user/brand-notes/final"

MARKS = [
    ("01-refract", "1 · ПРИЗМА + ЛУЧ", "физика преломления"),
    ("02-displace", "2 · СДВИГ", "треугольник разрезан и сдвинут"),
    ("03-aperture", "3 · АПЕРТУРА", "полый треугольник, самый чистый"),
]

PLATE = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96">
  <defs>
    <linearGradient id="plate" x1="0.15" y1="0" x2="0.85" y2="1">
      <stop offset="0" stop-color="#26262e"/><stop offset="1" stop-color="#0d0d11"/>
    </linearGradient>
  </defs>
  <rect width="96" height="96" rx="21.5" fill="url(#plate)"/>
  <rect x="0.35" y="0.35" width="95.3" height="95.3" rx="21.2" fill="none"
        stroke="#ffffff" stroke-opacity="0.10" stroke-width="0.7"/>
  <g transform="translate(48 {ty}) scale(0.60) translate(-48 -48)">{body}</g>
</svg>
"""


def inner_body(svg: str) -> str:
    body = svg.split(">", 1)[1].rsplit("</svg>", 1)[0]
    return re.sub(r"<title>.*?</title>", "", body, flags=re.S)


def render(svg_path: str, size: int, out_path: str) -> None:
    subprocess.run(
        [
            "node",
            "-e",
            "import('/tmp/tools/node_modules/@resvg/resvg-js/index.js').then(({Resvg})=>{"
            "const fs=require('fs');const s=fs.readFileSync(process.argv[1],'utf8');"
            "fs.writeFileSync(process.argv[3], new Resvg(s,{fitTo:{mode:'width',value:Number(process.argv[2])}}).render().asPng());});",
            svg_path,
            str(size),
            out_path,
        ],
        check=True,
    )


def sh(cmd: list[str]) -> None:
    subprocess.run(cmd, check=True)


for name, label, _ in MARKS:
    mark = open(f"{OUT}/{name}.svg").read()
    plate_path = f"{OUT}/{name}-plate.svg"
    open(plate_path, "w").write(PLATE.format(ty=48, body=inner_body(mark)))
    for size in (256, 96, 64, 32, 22, 16):
        render(f"{OUT}/{name}.svg", size, f"{OUT}/r/{name}-{size}.png")
    render(plate_path, 96, f"{OUT}/r/{name}-plate.png")

# ---------------------------------------------------------------- contact sheet
os.makedirs(f"{OUT}/r", exist_ok=True)
cells = []
for name, label, tagline in MARKS:
    big = f"{OUT}/r/{name}-big.png"
    label_png = f"{OUT}/r/{name}-label.png"
    row = f"{OUT}/r/{name}-row.png"
    tag_png = f"{OUT}/r/{name}-tag.png"
    cell = f"{OUT}/r/cell-{name}.png"

    sh(["convert", "-size", "330x268", "xc:#14141a",
        f"{OUT}/r/{name}-256.png", "-gravity", "center", "-composite", big])
    sh(["convert", "-size", "330x30", "xc:#14141a", "-gravity", "center",
        "-fill", "#eceaf2", "-font", "DejaVu-Sans-Bold", "-pointsize", "19",
        "-annotate", "+0+0", label, label_png])
    sh(["convert", "-size", "330x116", "xc:#14141a",
        f"{OUT}/r/{name}-plate.png", "-geometry", "+8+6", "-composite",
        f"{OUT}/r/{name}-64.png", "-geometry", "+120+26", "-composite",
        f"{OUT}/r/{name}-32.png", "-geometry", "+200+42", "-composite",
        f"{OUT}/r/{name}-22.png", "-geometry", "+244+47", "-composite",
        f"{OUT}/r/{name}-16.png", "-geometry", "+276+50", "-composite",
        "-gravity", "southwest", "-fill", "#6f6f7c", "-font", "DejaVu-Sans",
        "-pointsize", "10", "-annotate", "+10+4",
        "\u0432 \u043f\u043b\u0430\u0448\u043a\u0435   64   32   22   16", row])
    sh(["convert", "-size", "330x26", "xc:#14141a", "-gravity", "center",
        "-fill", "#8f8f9c", "-font", "DejaVu-Sans", "-pointsize", "13",
        "-annotate", "+0+0", tagline, tag_png])
    sh(["convert", big, label_png, row, tag_png, "-background", "#14141a",
        "-gravity", "center", "-append", cell])
    cells.append(cell)

sh(
    ["montage", *cells, "-tile", "3x1", "-geometry", "+10+10", "-background", "#1b1b22",
     f"{OUT}/sheet.png"]
)
print("built", len(cells), "cells ->", f"{OUT}/sheet.png")
