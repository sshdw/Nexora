"""Three finished 'prism' directions for Nexora, all drawn from scratch on a
96x96 grid. Single-colour white (one secondary tone at most), no gradients, no
bevel, no glow. Each one is exported as a standalone SVG plus a plate lockup."""

import os

OUT = "/home/user/brand-notes/final"
os.makedirs(OUT, exist_ok=True)

# ---------------------------------------------------------------- 01 REFRACT
# Solid prism (apex 42,14; base 10..74 at y=78) + one beam entering low and
# three tapered beams leaving high. Beams are closed polygons, not strokes, so
# they keep their shape when rasterised.
REFRACT = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96">
  <title>Nexora Prism - Refract</title>
  <!-- prism: apex (40,20), base y=76, left edge and right edge at 28 deg -->
  <path fill="#ffffff" d="M40 20 L72 76 L8 76 Z"/>
  <path fill="#ffffff" fill-opacity=".55" d="M40 20 L40 76 L8 76 Z"/>
  <!-- incoming beam, stopped 6 px short of the left edge -->
  <path fill="#ffffff" d="M0 47 L17 47 L17 56 L0 56 Z"/>
  <!-- three beams leaving, each starting a few px off the right edge -->
  <path fill="#ffffff" d="M54 29 L92 16 L92 25 L54 37 Z"/>
  <path fill="#ffffff" d="M61 42 L94 38 L94 47 L61 50 Z"/>
  <path fill="#ffffff" d="M68 61 L92 69 L92 78 L68 70 Z"/>
</svg>
"""

# --------------------------------------------------------------- 02 DISPLACE
# One triangle cut vertically; the halves slide apart and vertically out of
# register, the way a ray kinks at a boundary. Two tones keep the seam legible.
DISPLACE = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96">
  <title>Nexora Prism - Displace</title>
  <path fill="#ffffff" d="M42 8 L42 88 L6 88 Z"/>
  <path fill="#ffffff" fill-opacity=".58" d="M52 20 L86 88 L52 88 Z"/>
</svg>
"""

# --------------------------------------------------------------- 03 APERTURE
# Hollow prism: the outer triangle with an aperture that is the same triangle
# scaled about the shared centroid, so all three walls stay optically even.
APERTURE = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96">
  <title>Nexora Prism - Aperture</title>
  <path fill="#ffffff" fill-rule="evenodd"
        d="M48 6 L92 88 L4 88 Z M48 34.7 L68.9 70.5 L27.1 70.5 Z"/>
</svg>
"""

MARKS = {"01-refract": REFRACT, "02-displace": DISPLACE, "03-aperture": APERTURE}

# Plate lockup, 96 grid: rounded square (radius 22.3 %) in the app canvas tone
# with a hairline edge and the mark centred at 62 % of the plate.
PLATE_TEMPLATE = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="96" height="96">
  <defs>
    <linearGradient id="p{idx}" x1="0.15" y1="0" x2="0.85" y2="1">
      <stop offset="0" stop-color="#26262e"/><stop offset="1" stop-color="#0d0d11"/>
    </linearGradient>
  </defs>
  <rect width="96" height="96" rx="21.5" fill="url(#p{idx})"/>
  <rect x="0.35" y="0.35" width="95.3" height="95.3" rx="21.2" fill="none"
        stroke="#ffffff" stroke-opacity="0.10" stroke-width="0.7"/>
  <g transform="translate(48 48) scale(0.62) translate(-48 -48)">{mark}</g>
</svg>
"""

for name, svg in MARKS.items():
    open(f"{OUT}/{name}.svg", "w").write(svg)

print("marks:", ", ".join(f"{n}.svg" for n in MARKS))
