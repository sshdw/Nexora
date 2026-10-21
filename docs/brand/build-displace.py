"""02 — DISPLACE, v2: премиальная подача в духе референса.

Геометрия (сетка 96x96, габарит 4..92 × 10..86, центр 48/48):
  левый осколок  — прямоугольный треугольник, катет по срезу x=40
  правый осколок — тот же треугольник, сдвинут на (+16, −8) по срезу x=56
Обе половины равны по площади: сдвиг читается как преломление на границе,
а не как «две разные фигуры».

Слои премиальной версии:
  1. тело        — вертикальный градиент по каждой грани (верх светлый)
  2. кромка      — волосяной блик по срезу + по гипотенузе (полированное стекло)
  3. внутренний  — узкая тень у среза, даёт толщину материала
  4. подложка    — мягкая тень под знаком и световое пятно на плашке

Отдельно: плоская версия (печать/favicon) и версия для светлой темы.
"""
import pathlib

OUT = pathlib.Path(__file__).parent
OUT.mkdir(exist_ok=True)

LEFT = "M40 18 L40 86 L4 86 Z"
RIGHT = "M56 10 L92 78 L56 78 Z"

# --------------------------------------------------------------- 01 плоский
FLAT = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96" role="img" aria-label="Nexora Prism">
  <title>Nexora Prism — Displace (flat)</title>
  <!-- Плоская версия: один цвет + один тон. Печать, favicon, 16-24 px. -->
  <path fill="#ffffff" d="{LEFT}"/>
  <path fill="#ffffff" fill-opacity=".55" d="{RIGHT}"/>
</svg>
"""

# --------------------------------------------------------------- 02 гранёный
FACET = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96" role="img" aria-label="Nexora Prism">
  <title>Nexora Prism — Displace</title>
  <defs>
    <linearGradient id="faceA" x1="0.15" y1="0" x2="0.85" y2="1">
      <stop offset="0" stop-color="#ffffff"/>
      <stop offset="0.42" stop-color="#f6f7fb"/>
      <stop offset="1" stop-color="#b9bfd2"/>
    </linearGradient>
    <linearGradient id="faceB" x1="0.85" y1="0" x2="0.15" y2="1">
      <stop offset="0" stop-color="#dfe3f0"/>
      <stop offset="0.45" stop-color="#9aa0b8"/>
      <stop offset="1" stop-color="#555a70"/>
    </linearGradient>
    <!-- Тень у среза: 3 px, гаснет к нулю — материал «толщиной». -->
    <linearGradient id="cutA" x1="1" y1="0" x2="0" y2="0">
      <stop offset="0" stop-color="#5a6076" stop-opacity=".45"/>
      <stop offset="1" stop-color="#5a6076" stop-opacity="0"/>
    </linearGradient>
    <linearGradient id="cutB" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#5a6076" stop-opacity=".45"/>
      <stop offset="1" stop-color="#5a6076" stop-opacity="0"/>
    </linearGradient>
    <clipPath id="clipA"><path d="{LEFT}"/></clipPath>
    <clipPath id="clipB"><path d="{RIGHT}"/></clipPath>
  </defs>

  <!-- Тело -->
  <path fill="url(#faceA)" d="{LEFT}"/>
  <path fill="url(#faceB)" d="{RIGHT}"/>

  <!-- Внутренняя тень у срезов (внутри фигур, обрезана по их контуру) -->
  <g clip-path="url(#clipA)">
    <rect x="34" y="18" width="6" height="68" fill="url(#cutA)"/>
  </g>
  <g clip-path="url(#clipB)">
    <rect x="56" y="10" width="6" height="68" fill="url(#cutB)"/>
  </g>

  <!-- Кромки: срез + гипотенуза. Тонко, ровно на границе фигуры. -->
  <path stroke="#ffffff" stroke-opacity=".9" stroke-width="1.4" d="M40 18 L40 86"/>
  <path stroke="#ffffff" stroke-opacity=".5" stroke-width="1" d="M40 18 L4 86"/>
  <path stroke="#ffffff" stroke-opacity=".62" stroke-width="1.1" d="M56 10 L56 78"/>
  <path stroke="#ffffff" stroke-opacity=".3" stroke-width="1" d="M56 10 L92 78"/>
</svg>
"""

# --------------------------------------------------------------- 03 мелкий
SMALL = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96" role="img" aria-label="Nexora Prism">
  <title>Nexora Prism — Displace (small sizes)</title>
  <!-- До 24 px кромки и градиенты превращаются в шум: плотный силуэт без
       обработки, зазор чуть шире за счёт тонкой второй половины. -->
  <path fill="#ffffff" d="{LEFT}"/>
  <path fill="#ffffff" fill-opacity=".68" d="{RIGHT}"/>
</svg>
"""

# --------------------------------------------------------------- 04 светлая
ON_LIGHT = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96" role="img" aria-label="Nexora Prism">
  <title>Nexora Prism — Displace (on light)</title>
  <!-- Версия для светлой темы: тёмная грань на бумаге, тот же сдвиг. -->
  <path fill="#14141a" d="{LEFT}"/>
  <path fill="#14141a" fill-opacity=".45" d="{RIGHT}"/>
</svg>
"""

# --------------------------------------------------------------- 05 плашка
PLATE = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96" role="img" aria-label="Nexora Prism">
  <title>Nexora Prism — app icon</title>
  <defs>
    <linearGradient id="plateBody" x1="0.12" y1="0" x2="0.88" y2="1">
      <stop offset="0" stop-color="#31313b"/>
      <stop offset="0.5" stop-color="#1a1a21"/>
      <stop offset="1" stop-color="#0a0a0e"/>
    </linearGradient>
    <radialGradient id="plateGlow" cx="0.5" cy="0.45" r="0.5">
      <stop offset="0" stop-color="#ffffff" stop-opacity=".11"/>
      <stop offset="0.55" stop-color="#ffffff" stop-opacity=".03"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="faceA2" x1="0.15" y1="0" x2="0.85" y2="1">
      <stop offset="0" stop-color="#ffffff"/>
      <stop offset="0.42" stop-color="#f6f7fb"/>
      <stop offset="1" stop-color="#b9bfd2"/>
    </linearGradient>
    <linearGradient id="faceB2" x1="0.85" y1="0" x2="0.15" y2="1">
      <stop offset="0" stop-color="#dfe3f0"/>
      <stop offset="0.45" stop-color="#9aa0b8"/>
      <stop offset="1" stop-color="#555a70"/>
    </linearGradient>
    <clipPath id="clipA2"><path d="{LEFT}"/></clipPath>
    <clipPath id="clipB2"><path d="{RIGHT}"/></clipPath>
    <filter id="soft" x="-40%" y="-40%" width="180%" height="180%">
      <feGaussianBlur stdDeviation="2.4"/>
    </filter>
  </defs>

  <rect width="96" height="96" rx="21.5" fill="url(#plateBody)"/>
  <rect width="96" height="96" rx="21.5" fill="url(#plateGlow)"/>
  <path d="M24 1.2 H72" stroke="#ffffff" stroke-opacity=".18" stroke-width="1.3"
        stroke-linecap="round"/>
  <rect x="0.4" y="0.4" width="95.2" height="95.2" rx="21.2" fill="none"
        stroke="#ffffff" stroke-opacity=".08" stroke-width="0.8"/>

  <g transform="translate(48 49) scale(0.66) translate(-48 -48)">
    <!-- мягкая тень под знаком: отрывает его от плашки -->
    <g filter="url(#soft)" opacity=".4">
      <path fill="#000000" d="{LEFT}" transform="translate(1.2 2.6)"/>
      <path fill="#000000" d="{RIGHT}" transform="translate(1.2 2.6)"/>
    </g>

    <path fill="url(#faceA2)" d="{LEFT}"/>
    <path fill="url(#faceB2)" d="{RIGHT}"/>

    <g clip-path="url(#clipA2)">
      <rect x="34" y="18" width="7" height="68" fill="#5a6076" opacity=".35"/>
    </g>
    <g clip-path="url(#clipB2)">
      <rect x="56" y="10" width="7" height="68" fill="#5a6076" opacity=".35"/>
    </g>

    <!-- мягкое свечение по срезам: тот самый «полированный» вид -->
    <g filter="url(#soft)" opacity=".5">
      <path stroke="#ffffff" stroke-width="3" d="M40 18 L40 86"/>
      <path stroke="#ffffff" stroke-width="2.4" d="M56 10 L56 78"/>
    </g>

    <path stroke="#ffffff" stroke-opacity=".95" stroke-width="2.2" d="M40 18 L40 86"/>
    <path stroke="#ffffff" stroke-opacity=".55" stroke-width="1.6" d="M40 18 L4 86"/>
    <path stroke="#ffffff" stroke-opacity=".7" stroke-width="1.8" d="M56 10 L56 78"/>
    <path stroke="#ffffff" stroke-opacity=".35" stroke-width="1.6" d="M56 10 L92 78"/>
  </g>
</svg>
"""

files = {
    "02-displace-flat.svg": FLAT,
    "02-displace.svg": FACET,
    "02-displace-small.svg": SMALL,
    "02-displace-onlight.svg": ON_LIGHT,
    "02-displace-plate.svg": PLATE,
}
for name, svg in files.items():
    (OUT / name).write_text(svg)
print("wrote", ", ".join(files))
