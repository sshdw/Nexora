/**
 * Shared sRGB/OKLCH helpers for the theme contract tests.
 *
 * These are TEST helpers on purpose. The app never needs colour
 * math at runtime, and shipping a colour-conversion library to
 * assert a stylesheet would be worse than keeping it here.
 *
 * Values are parsed out of the CSS text rather than imported, so
 * the assertion and the stylesheet cannot drift: this helper only
 * has to be correct about colour science, not about where the
 * theme lives.
 */

/** `oklch(0.145 0.004 264)`, optionally with a `/ <alpha>` suffix. */
export interface Oklch {
  /** Perceptual lightness, 0..1. */
  l: number;
  /** Chroma, 0..~0.4. */
  c: number;
  /** Hue in degrees. */
  h: number;
  /** Alpha in 0..1; defaults to 1. */
  a: number;
}

const OKLCH_PATTERN =
  /^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*(?:\/\s*([\d.]+%?)\s*)?\)$/;

export function parseOklch(value: string): Oklch | null {
  const match = OKLCH_PATTERN.exec(value.trim());
  if (!match) return null;
  const [, l, c, h, alpha] = match;
  return {
    l: parseFloat(l),
    c: parseFloat(c),
    h: parseFloat(h),
    a: alpha === undefined ? 1 : parseAlpha(alpha),
  };
}

function parseAlpha(raw: string): number {
  if (raw.endsWith("%")) return parseFloat(raw) / 100;
  const n = parseFloat(raw);
  // `/ 1.5` is invalid CSS (alpha is clamped to 0..1 by the
  // compositor), and silently passing 1.5 through would make a
  // contrast assertion read a colour that cannot exist. Clamp.
  return Math.min(1, n > 1 ? n / 100 : n);
}

/**
 * OKLCH → linear sRGB (Björn Ottosson's matrices). Channels may fall
 * outside 0..1 for out-of-gamut colours; `linearToSrgb` clamps.
 */
function oklchToLinearSrgb({ l, c, h }: Oklch): [number, number, number] {
  const hRad = (h * Math.PI) / 180;
  const a = c * Math.cos(hRad);
  const b = c * Math.sin(hRad);

  const l_ = l + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = l - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = l - 0.0894841775 * a - 1.291485548 * b;

  const l3 = l_ * l_ * l_;
  const m3 = m_ * m_ * m_;
  const s3 = s_ * s_ * s_;

  return [
    4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
    -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
    -0.0041960863 * l3 - 0.7034186147 * m3 + 1.707614701 * s3,
  ];
}

/**
 * sRGB-encoded channel (0..1) → linear-light. Used only for hex
 * input, which arrives already gamma-encoded.
 */
export function srgbToLinear(channel: number): number {
  const clamped = Math.min(1, Math.max(0, channel));
  return clamped <= 0.04045
    ? clamped / 12.92
    : ((clamped + 0.055) / 1.055) ** 2.4;
}

/**
 * Linear-light → sRGB-encoded.
 *
 * The 0.0031308 threshold is the ENCODE-side knee of the sRGB curve
 * (0.04045 is its DECODE-side twin). Using the decode knee here made
 * `srgbToLinear(linearToSrgb(x))` lossy for linear x in
 * (0.0031308, 0.04045] — e.g. 0.0238 round-tripped to 0.0769 — which
 * distorted every dark-surface value compositedLuminance blends.
 * (Review 06 D-02 found this while wiring the hex-token chroma sweep.)
 */
export function linearToSrgb(channel: number): number {
  const clamped = Math.min(1, Math.max(0, channel));
  return clamped <= 0.0031308
    ? clamped * 12.92
    : 1.055 * clamped ** (1 / 2.4) - 0.055;
}

/** `#rrggbb` → `[r, g, b]` in 0..1 sRGB. */
export function parseHexColor(hex: string): [number, number, number] {
  const value = hex.trim().replace("#", "");
  const expand =
    value.length === 3
      ? value
          .split("")
          .map((c) => c + c)
          .join("")
      : value;
  return [
    parseInt(expand.slice(0, 2), 16) / 255,
    parseInt(expand.slice(2, 4), 16) / 255,
    parseInt(expand.slice(4, 6), 16) / 255,
  ];
}

/** A parsed colour with its alpha, 0..1. */
export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

/**
 * `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` → channels in 0..1 sRGB.
 * Null for anything else, so the caller can classify non-colours.
 */
export function parseHexRgba(hex: string): Rgba | null {
  const match = /^#([0-9a-fA-F]{3,8})$/.exec(hex.trim());
  if (!match) return null;
  let value = match[1];
  if (value.length === 3 || value.length === 4) {
    value = value
      .split("")
      .map((c) => c + c)
      .join("");
  }
  if (value.length !== 6 && value.length !== 8) return null;
  const channel = (i: number) => parseInt(value.slice(i, i + 2), 16) / 255;
  return {
    r: channel(0),
    g: channel(2),
    b: channel(4),
    a: value.length === 8 ? channel(6) : 1,
  };
}

/** `rgb()`/`rgba()` with comma-separated channels → channels in 0..1. */
export function parseRgbaFunctional(value: string): Rgba | null {
  const match =
    /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+))?\s*\)$/.exec(
      value.trim()
    );
  if (!match) return null;
  return {
    r: Number(match[1]) / 255,
    g: Number(match[2]) / 255,
    b: Number(match[3]) / 255,
    a: match[4] === undefined ? 1 : Math.min(1, Math.max(0, Number(match[4]))),
  };
}

/** OKLab components (Ottosson). `l` is perceptual lightness; the
 *  chroma of a colour is `Math.hypot(a, b)`. */
export interface Oklab {
  l: number;
  a: number;
  b: number;
}

/**
 * sRGB-ENCODED channels → OKLab (Björn Ottosson's inverse matrices —
 * the exact reverse of `oklchToLinearSrgb`'s forward path).
 */
export function srgbToOklab(rgb: [number, number, number]): Oklab {
  const [lr, lg, lb] = rgb.map(srgbToLinear);
  const l_ = Math.cbrt(
    0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb
  );
  const m_ = Math.cbrt(
    0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb
  );
  const s_ = Math.cbrt(
    0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb
  );
  return {
    l: 0.2104542553 * l_ + 0.793617785 * m_ - 0.0040720468 * s_,
    a: 1.9779984951 * l_ - 2.428592205 * m_ + 0.4505937099 * s_,
    b: 0.0259040371 * l_ + 0.7827717662 * m_ - 0.808675766 * s_,
  };
}

/** OKLCH-style chroma of an OKLab colour: the (a, b) radius. */
export function oklabChroma(lab: Oklab): number {
  return Math.hypot(lab.a, lab.b);
}

/**
 * Alpha-composite `color` over an opaque `backdrop`, both already in
 * 0..1 sRGB-ENCODED channels — the space CSS alpha compositing
 * happens in (same convention as `compositedLuminance`).
 */
export function compositedSrgb(
  color: Rgba,
  backdrop: [number, number, number]
): [number, number, number] {
  const blend = (fg: number, bg: number) => color.a * fg + (1 - color.a) * bg;
  return [
    blend(color.r, backdrop[0]),
    blend(color.g, backdrop[1]),
    blend(color.b, backdrop[2]),
  ];
}

/**
 * OKLCH → clamped sRGB-ENCODED channels. The backdrop half of the
 * composited-chroma sweep: an oklch theme role (the canvas) becomes
 * the channels an rgba/hex literal alpha-blends over.
 */
export function oklchToSrgbChannels(color: Oklch): [number, number, number] {
  return oklchToLinearSrgb(color).map(linearToSrgb) as [number, number, number];
}

/**
 * WCAG 2.x relative luminance of an opaque colour.
 *
 * WCAG's coefficients are defined against LINEAR light, and
 * `oklchToLinearSrgb` already returns linear channels — so the
 * weighted sum goes straight onto them. (Applying a gamma encode
 * here, as the obvious-looking implementation does, double-encodes
 * and quietly flattens every ratio in this file.)
 */
export function relativeLuminance(color: Oklch): number {
  const [r, g, b] = oklchToLinearSrgb(color);
  const toLinear = (channel: number): number =>
    Math.min(1, Math.max(0, channel));
  return 0.2126 * toLinear(r) + 0.7152 * toLinear(g) + 0.0722 * toLinear(b);
}

/**
 * WCAG 2.x relative luminance of a translucent colour as the
 * browser composites it over an opaque backdrop.
 *
 * The blend happens in sRGB-ENCODED space, because that is where
 * CSS alpha compositing happens. Blending in linear light
 * understates every translucent border by a wide margin — which
 * is exactly the kind of quiet wrongness a contrast gate exists
 * to prevent.
 */
export function compositedLuminance(color: Oklch, backdrop: Oklch): number {
  const a = color.a;
  const fg = oklchToLinearSrgb(color).map(linearToSrgb);
  const bg = oklchToLinearSrgb(backdrop).map(linearToSrgb);
  const blended = fg.map((channel, i) => a * channel + (1 - a) * bg[i]);
  return (
    0.2126 * srgbToLinear(blended[0]) +
    0.7152 * srgbToLinear(blended[1]) +
    0.0722 * srgbToLinear(blended[2])
  );
}

/** WCAG 2.x contrast ratio. Both colours must be opaque. */
export function contrastRatio(a: Oklch, b: Oklch): number {
  const l1 = relativeLuminance(a);
  const l2 = relativeLuminance(b);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

/** Contrast ratio of a translucent `color` over an opaque `backdrop`. */
export function contrastOver(color: Oklch, backdrop: Oklch): number {
  return ratioBetween(
    compositedLuminance(color, backdrop),
    relativeLuminance(backdrop)
  );
}

/**
 * Contrast ratio of an opaque `text` over a translucent `wash`
 * (which carries its own alpha) sitting on `backdrop` — the
 * pairing shadcn's destructive and ghost variants actually paint.
 */
export function contrastOnWash(
  text: Oklch,
  wash: Oklch,
  backdrop: Oklch
): number {
  return ratioBetween(
    relativeLuminance(text),
    compositedLuminance(wash, backdrop)
  );
}

function ratioBetween(l1: number, l2: number): number {
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}
