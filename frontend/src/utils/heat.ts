/**
 * The heat ramp: yellow through orange to red, the same in every skin — a
 * heatmap reads by its hue, so it does not follow the theme tokens. The GPU
 * graph draws it as a glow around each card (heatRgb); the minimap and the
 * Vue Flow fallback take it as a CSS color in ten steps (heatColor), so
 * their color caches stay small. Notes outside the index (anchors,
 * tombstones) get null.
 */
const STOPS: [number, number, number][] = [
    [250, 204, 21], // yellow
    [249, 115, 22], // orange
    [220, 38, 38], // red
]

/** The ramp at `heat` (clamped to 0..1), each channel 0..1. */
export function heatRgb(heat: number): [number, number, number] {
    const t = Math.min(1, Math.max(0, heat)) * (STOPS.length - 1)
    const i = Math.min(STOPS.length - 2, Math.floor(t))
    const k = t - i
    const a = STOPS[i]!
    const b = STOPS[i + 1]!
    return [0, 1, 2].map((c) => (a[c]! + (b[c]! - a[c]!) * k) / 255) as [number, number, number]
}

export function heatColor(heat: number | null): string | null {
    if (heat == null) return null
    const [r, g, b] = heatRgb(Math.round(Math.min(1, Math.max(0, heat)) * 10) / 10)
    return `rgb(${Math.round(r * 255)}, ${Math.round(g * 255)}, ${Math.round(b * 255)})`
}
