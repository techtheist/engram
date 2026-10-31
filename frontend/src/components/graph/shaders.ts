/**
 * GLSL for the WebGL2 canvas (0.10.1). Everything is premultiplied alpha.
 * Units: world = the graph's CSS px at zoom 1 (Vue Flow's flow coords);
 * `u_cam` is (translate x, translate y, zoom) in CSS px, `u_dpr` the
 * device pixel ratio the canvas renders at.
 */

import { FOCUS_DIM, FOCUS_EASE, FOCUS_FALLOFF } from './focus'

const HEADER = /* glsl */ `#version 300 es
precision highp float;
`

/** Full-screen triangle; no attributes. */
export const FULLSCREEN_VS = /* glsl */ `${HEADER}
out vec2 v_uv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
`

/** Canvas color, the dot grid (Vue Flow Background: gap 22, size 1.4) and
 * the brand glow (two radial washes, transparent in the IDE themes). */
export const BACKGROUND_FS = /* glsl */ `${HEADER}
uniform vec4 u_bg;
uniform vec4 u_dots;
uniform vec4 u_glow1;
uniform vec4 u_glow2;
uniform vec3 u_cam;
uniform float u_dpr;
uniform vec2 u_viewPx;
uniform float u_rem;
out vec4 o;

vec3 glow(vec3 c, vec4 g, vec2 p, vec2 center, float radius) {
    float t = clamp(length(p - center) / (radius * 0.7), 0.0, 1.0);
    return mix(c, g.rgb, g.a * (1.0 - t));
}

void main() {
    vec2 view = u_viewPx / u_dpr;
    vec2 p = vec2(gl_FragCoord.x, u_viewPx.y - gl_FragCoord.y) / u_dpr;
    vec3 c = u_bg.rgb;
    float gap = 22.0 * u_cam.z;
    float r = 0.7 * u_cam.z;
    float a;
    if (gap * u_dpr < 5.0) {
        // Denser than the eye can split: the grid's average tone.
        a = min(1.0, 3.14159 * r * r / (gap * gap));
    } else {
        vec2 q = mod(p - u_cam.xy, gap);
        float d = length(q - vec2(r));
        a = clamp((r - d) * u_dpr + 0.5, 0.0, 1.0) * clamp(r * u_dpr * 2.0, 0.0, 1.0);
    }
    c = mix(c, u_dots.rgb, a * u_dots.a);
    c = glow(c, u_glow2, p, view * vec2(0.82, 0.88), 70.0 * u_rem);
    c = glow(c, u_glow1, p, view * vec2(0.18, 0.12), 60.0 * u_rem);
    o = vec4(c, 1.0);
}
`

/**
 * Edges: one instance per edge, a fixed strip of EDGE_SEGS segments built
 * from gl_VertexID; the VS evaluates Vue Flow's bezier (handles out right,
 * in left, curvature 0.25) — so moving a node re-uploads 12 floats per
 * edge, not a tessellated mesh. Arc length (for dashes) only when dashed.
 */
export const EDGE_SEGS = 40

/**
 * Selection focus (focus.ts): an edge's focus is -1 (dimmed), 0 (nothing
 * selected) or its hop ring around the selection (focusWidth's widths).
 * Each instance carries its previous focus and when it changed, and the
 * shader eases between them, so a new selection needs no frame-by-frame
 * re-upload.
 */
const FOCUS = /* glsl */ `
const float FOCUS_DIM = ${FOCUS_DIM.toFixed(2)};
vec2 focusOf(float s) {
    if (s < -0.5) return vec2(1.0, FOCUS_DIM);
    if (s < 0.5) return vec2(1.0);
    return vec2(1.0 + pow(${FOCUS_FALLOFF.toFixed(2)}, s - 1.0), 1.0);
}
/** (width multiplier, alpha) at time t for focus (from, to, since). */
vec2 focusAt(vec3 f, float t) {
    float k = clamp((t - f.z) / ${FOCUS_EASE.toFixed(2)}, 0.0, 1.0);
    return mix(focusOf(f.x), focusOf(f.y), k * k * (3.0 - 2.0 * k));
}
`

export const EDGE_VS = /* glsl */ `${HEADER}
layout(location = 0) in vec4 a_ends;  // sx, sy, tx, ty
layout(location = 1) in vec4 a_color; // straight rgba
layout(location = 2) in vec2 a_style; // kind bits (1 dashed, 2 animated), appear start
layout(location = 3) in vec3 a_focus; // previous focus, focus, since (see FOCUS)
uniform vec3 u_cam;
uniform vec2 u_view;
uniform float u_dpr;
uniform float u_width;
uniform float u_time;
/** Segments actually drawn (≤ EDGE_SEGS): fewer when zoomed out. */
uniform float u_segs;
/** 1 when zoomed out to bars: every edge solid and still — no dashes to move. */
uniform float u_static;
out float v_d;
out float v_half;
out float v_alpha;
out float v_s;
out vec4 v_color;
flat out int v_kind;
${FOCUS}
vec2 bez(float t, vec2 p0, vec2 p1, vec2 p2, vec2 p3) {
    float mt = 1.0 - t;
    return mt * mt * mt * p0 + 3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t * p3;
}

void main() {
    int i = gl_VertexID / 2;
    float side = (gl_VertexID % 2 == 0) ? -1.0 : 1.0;
    float t = float(i) / u_segs;
    vec2 p0 = a_ends.xy;
    vec2 p3 = a_ends.zw;
    float dx = p3.x - p0.x;
    float off = dx >= 0.0 ? 0.5 * dx : 6.25 * sqrt(-dx);
    vec2 p1 = vec2(p0.x + off, p0.y);
    vec2 p2 = vec2(p3.x - off, p3.y);
    vec2 pos = bez(t, p0, p1, p2, p3);
    float mt = 1.0 - t;
    vec2 tan = 3.0 * mt * mt * (p1 - p0) + 6.0 * mt * t * (p2 - p1) + 3.0 * t * t * (p3 - p2);
    if (dot(tan, tan) < 1e-8) tan = p3 - p0 + vec2(1e-3, 0.0);
    tan = normalize(tan);

    int kind = u_static > 0.5 ? 0 : int(a_style.x + 0.5);
    float s = 0.0;
    if (kind != 0) {
        vec2 prev = p0;
        for (int k = 1; k <= ${EDGE_SEGS}; k++) {
            if (k > i) break;
            vec2 q = bez(float(k) / u_segs, p0, p1, p2, p3);
            s += length(q - prev);
            prev = q;
        }
    }

    vec2 focus = focusAt(a_focus, u_time);
    float px = u_width * focus.x * u_cam.z * u_dpr;
    float draw = max(px, 1.0);
    float ext = draw * 0.5 + 1.0;
    vec2 world = pos + vec2(-tan.y, tan.x) * side * ext / (u_cam.z * u_dpr);
    vec2 screen = world * u_cam.z + u_cam.xy;
    vec2 clip = screen / u_view * 2.0 - 1.0;
    gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
    v_d = side * ext;
    v_half = draw * 0.5;
    v_alpha = min(px, 1.0) * clamp((u_time - a_style.y) / 0.32, 0.0, 1.0) * focus.y;
    v_s = s;
    v_color = a_color;
    v_kind = kind;
}
`

export const EDGE_FS = /* glsl */ `${HEADER}
uniform float u_time;
in float v_d;
in float v_half;
in float v_alpha;
in float v_s;
in vec4 v_color;
flat in int v_kind;
out vec4 o;
void main() {
    float a = clamp(v_half + 0.5 - abs(v_d), 0.0, 1.0) * v_alpha;
    if (v_kind != 0) {
        bool dashed = (v_kind & 1) != 0;
        bool animated = (v_kind & 2) != 0;
        float on = dashed ? 6.0 : 5.0;
        float pos = mod(v_s - (animated ? u_time * 20.0 : 0.0), 10.0);
        float aa = max(fwidth(v_s), 1e-3);
        a *= clamp((on - pos) / aa + 0.5, 0.0, 1.0) * clamp(pos / aa + 0.5, 0.0, 1.0);
    }
    float alpha = v_color.a * a;
    o = vec4(v_color.rgb * alpha, alpha);
}
`

/** Edge labels: instanced quads from the label atlas (mipmapped). */
export const LABEL_VS = /* glsl */ `${HEADER}
layout(location = 0) in vec2 a_corner;
layout(location = 1) in vec4 a_box; // center x, y, w, h (world)
layout(location = 2) in vec4 a_uv;
layout(location = 3) in float a_appear;
layout(location = 4) in vec3 a_focus;
uniform vec3 u_cam;
uniform vec2 u_view;
uniform float u_time;
out vec2 v_uv;
out float v_alpha;
${FOCUS}
void main() {
    vec2 world = a_box.xy + (a_corner - 0.5) * a_box.zw;
    vec2 screen = world * u_cam.z + u_cam.xy;
    vec2 clip = screen / u_view * 2.0 - 1.0;
    gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
    v_uv = mix(a_uv.xy, a_uv.zw, a_corner);
    v_alpha = clamp((u_time - a_appear) / 0.32, 0.0, 1.0) * focusAt(a_focus, u_time).y;
}
`

export const LABEL_FS = /* glsl */ `${HEADER}
uniform sampler2D u_tex;
in vec2 v_uv;
in float v_alpha;
out vec4 o;
void main() {
    o = texture(u_tex, v_uv) * v_alpha;
}
`

/**
 * Crowding heat (0.11.0): a soft glow around each crowded card, drawn on
 * the canvas under the edges. One instanced quad per card, grown by
 * `u_spread` on every side; its alpha is a gaussian of the distance to the
 * card's rounded rect, so it reads as a halo near and as a colored cloud
 * from far out. Overlapping halos stack — a crowded pile glows hotter.
 */
export const GLOW_VS = /* glsl */ `${HEADER}
layout(location = 0) in vec2 a_corner;
layout(location = 1) in vec4 a_rect;  // x, y, w, h (world)
layout(location = 2) in vec4 a_color; // straight rgb, intensity 0..1
uniform vec3 u_cam;
uniform vec2 u_view;
uniform float u_spread;
out vec2 v_local;
flat out vec2 v_size;
flat out vec4 v_color;
void main() {
    vec2 local = mix(vec2(-u_spread), a_rect.zw + u_spread, a_corner);
    vec2 screen = (a_rect.xy + local) * u_cam.z + u_cam.xy;
    vec2 clip = screen / u_view * 2.0 - 1.0;
    gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
    v_local = local;
    v_size = a_rect.zw;
    v_color = a_color;
}
`

export const GLOW_FS = /* glsl */ `${HEADER}
uniform float u_spread;
uniform float u_alpha;
uniform float u_radius;
in vec2 v_local;
flat in vec2 v_size;
flat in vec4 v_color;
out vec4 o;
float sdRound(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
void main() {
    float d = max(sdRound(v_local - v_size * 0.5, v_size * 0.5, u_radius), 0.0);
    // sigma = spread / 2.5: the halo has faded out by the quad's edge.
    float s = u_spread * 0.4;
    float a = exp(-(d * d) / (2.0 * s * s)) * v_color.a * u_alpha;
    o = vec4(v_color.rgb * a, a);
}
`

/**
 * Cards: one instanced quad each — shadow (CSS --shadow-md, Evan Wallace's
 * closed-form rounded-box gaussian), surface (frosted in the brand theme:
 * the blurred backdrop, saturate 180% contrast 105%, under the surface
 * color), the accent strip and its wash, the painted content texture, the
 * border, the two handles; the archived grayscale and the node-appear
 * spring over all of it.
 */
export const CARD_VS = /* glsl */ `${HEADER}
layout(location = 0) in vec2 a_corner;
layout(location = 1) in vec4 a_rect;   // x, y, w, h (world)
layout(location = 2) in vec4 a_accent; // straight rgba
layout(location = 3) in vec4 a_uv;     // u0, v0, u1, v1 in the atlas layer
layout(location = 4) in vec4 a_misc;   // layer, archived, appear start, has texture
layout(location = 5) in vec4 a_parts;  // title, body, refs, tags heights (px)
layout(location = 6) in vec4 a_heat;   // crowding tint: straight rgb, opacity
uniform vec3 u_cam;
uniform vec2 u_view;
uniform float u_time;
uniform float u_margin;
out vec2 v_local;
flat out vec2 v_size;
flat out vec4 v_accent;
flat out vec4 v_uv;
flat out vec4 v_misc;
flat out vec4 v_parts;
flat out vec4 v_heat;
flat out float v_alpha;
void main() {
    float t = clamp((u_time - a_misc.z) / 0.32, 0.0, 1.0);
    // cubic-bezier(0.34, 1.56, 0.64, 1) — the node-appear spring, as easeOutBack.
    float e = 1.0 + 2.70158 * pow(t - 1.0, 3.0) + 1.70158 * pow(t - 1.0, 2.0);
    float scale = mix(0.9, 1.0, e);
    float dy = mix(6.0, 0.0, e);
    vec2 local = mix(vec2(-u_margin), a_rect.zw + u_margin, a_corner);
    vec2 center = a_rect.xy + a_rect.zw * 0.5;
    vec2 world = center + (a_rect.xy + local - center) * scale + vec2(0.0, dy);
    vec2 screen = world * u_cam.z + u_cam.xy;
    vec2 clip = screen / u_view * 2.0 - 1.0;
    gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
    v_local = local;
    v_size = a_rect.zw;
    v_accent = a_accent;
    v_uv = a_uv;
    v_misc = a_misc;
    v_parts = a_parts;
    v_heat = a_heat;
    v_alpha = clamp(e, 0.0, 1.0);
}
`

export const CARD_FS = /* glsl */ `${HEADER}
precision highp sampler2DArray;
uniform sampler2DArray u_tex;
uniform sampler2D u_backdrop;
uniform vec4 u_surface;
uniform vec4 u_border;
uniform vec4 u_handleFill;
uniform vec4 u_handleRing;
uniform float u_radius;
uniform float u_borderW;
uniform float u_wash;
uniform float u_px;      // one device pixel in world units
uniform float u_glass;
uniform vec2 u_viewPx;
uniform float u_shadow;  // 0 when the shadow is under a device pixel
uniform float u_greek;   // 1 = text drawn as bars (far zoom), 0 = textures
uniform vec4 u_gk0;      // padX + border, content top, header height, gap
uniform vec4 u_gk1;      // title line height, body line height, content width, -
uniform vec4 u_textPrimary;
uniform vec4 u_textSecondary;
uniform vec4 u_textTertiary;
uniform vec4 u_primary;
in vec2 v_local;
flat in vec2 v_size;
flat in vec4 v_accent;
flat in vec4 v_uv;
flat in vec4 v_misc;
flat in vec4 v_parts;
flat in vec4 v_heat;
flat in float v_alpha;
out vec4 o;

float sdRound(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
vec4 over(vec4 top, vec4 under) { return top + under * (1.0 - top.a); }
vec4 pm(vec4 c) { return vec4(c.rgb * c.a, c.a); }

float gaussian(float x, float sigma) {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (2.5066283 * sigma);
}
vec2 erf2(vec2 x) {
    vec2 s = sign(x), a = abs(x);
    x = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    x *= x;
    return s - s / (x * x);
}
float shadowX(float x, float y, float sigma, float corner, vec2 halfSize) {
    float delta = min(halfSize.y - corner - abs(y), 0.0);
    float curved = halfSize.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    vec2 integral = 0.5 + 0.5 * erf2((x + vec2(-curved, curved)) * (0.7071068 / sigma));
    return integral.y - integral.x;
}
float boxShadow(vec2 size, vec2 point, float sigma, float corner) {
    vec2 halfSize = size * 0.5;
    point -= halfSize;
    float low = point.y - halfSize.y;
    float high = point.y + halfSize.y;
    float start = clamp(-3.0 * sigma, low, high);
    float end = clamp(3.0 * sigma, low, high);
    float step = (end - start) / 4.0;
    float y = start + step * 0.5;
    float value = 0.0;
    for (int i = 0; i < 4; i++) {
        value += shadowX(point.x, point.y - y, sigma, corner, halfSize) * gaussian(y, sigma) * step;
        y += step;
    }
    return value;
}

float pill(vec2 p, vec2 at, vec2 sz) {
    float d = sdRound(p - at - sz * 0.5, sz * 0.5, sz.y * 0.5);
    return clamp(0.5 - d / u_px, 0.0, 1.0);
}

/** The card's text as bars — what it looks like below readable size, and
 * the stand-in while a card's texture is still being painted. */
vec4 greek(vec2 p) {
    float x0 = u_gk0.x;
    float y = u_gk0.y;
    float cw = u_gk1.z;
    float seed = fract(sin(dot(v_misc.zz + v_size, vec2(12.9898, 78.233))) * 43758.5453);
    vec4 col = vec4(0.0);
    float hy = y + u_gk0.z * 0.5 - 6.0;
    col = over(pm(vec4(v_accent.rgb, 0.4)) * pill(p, vec2(x0, hy), vec2(56.0, 12.0)), col);
    col = over(pm(vec4(u_textTertiary.rgb, 0.3)) * pill(p, vec2(x0 + 62.0, hy), vec2(40.0, 12.0)), col);
    col = over(pm(vec4(u_textTertiary.rgb, 0.3)) * pill(p, vec2(x0 + cw - 62.0, hy), vec2(62.0, 12.0)), col);
    y += u_gk0.z + u_gk0.w;
    float tlh = u_gk1.x;
    float tl = floor(v_parts.x / tlh + 0.5);
    for (int i = 0; i < 5; i++) {
        float fi = float(i);
        if (fi >= tl) break;
        float w = (fi == tl - 1.0) ? cw * (0.45 + 0.35 * seed) : cw * 0.95;
        col = over(pm(vec4(u_textPrimary.rgb, 0.75)) * pill(p, vec2(x0, y + fi * tlh + tlh * 0.5 - 4.5), vec2(w, 9.0)), col);
    }
    y += v_parts.x;
    if (v_parts.y > 0.0) {
        y += u_gk0.w;
        float blh = u_gk1.y;
        if (p.y < y + v_parts.y) {
            for (int i = 0; i < 5; i++) {
                float fi = float(i);
                float w = cw * (0.78 + 0.18 * fract(seed * (fi + 3.0) * 7.31));
                col = over(pm(vec4(u_textSecondary.rgb, 0.45)) * pill(p, vec2(x0, y + fi * blh + blh * 0.5 - 3.5), vec2(w, 7.0)), col);
            }
        }
        y += v_parts.y;
    }
    if (v_parts.z > 0.0) {
        y += u_gk0.w;
        col = over(pm(vec4(u_textTertiary.rgb, 0.3)) * pill(p, vec2(x0, y + 1.0), vec2(cw * 0.45, v_parts.z - 2.0)), col);
        y += v_parts.z;
    }
    if (v_parts.w > 0.0) {
        y += u_gk0.w;
        col = over(pm(vec4(u_primary.rgb, 0.35)) * pill(p, vec2(x0, y + 1.0), vec2(64.0, v_parts.w - 2.0)), col);
        col = over(pm(vec4(u_primary.rgb, 0.35)) * pill(p, vec2(x0 + 70.0, y + 1.0), vec2(44.0, v_parts.w - 2.0)), col);
    }
    return col;
}

void main() {
    vec2 p = v_local;
    vec2 size = v_size;
    float d = sdRound(p - size * 0.5, size * 0.5, u_radius);
    if (d > 44.0) discard;
    float cover = clamp(0.5 - d / u_px, 0.0, 1.0);
    vec4 col = vec4(0.0);

    if (cover < 1.0 && u_shadow > 0.5) {
        // box-shadow paints outside the border box only.
        float s1 = boxShadow(size, p - vec2(0.0, 8.0), 12.0, u_radius) * 0.28;
        float s2 = boxShadow(size, p - vec2(0.0, 2.0), 4.0, u_radius) * 0.18;
        float sh = (1.0 - (1.0 - s1) * (1.0 - s2)) * (1.0 - cover);
        col = vec4(0.0, 0.0, 0.0, sh);
    }

    if (cover > 0.0) {
        vec4 surf = pm(u_surface);
        vec4 body;
        if (u_glass > 0.5) {
            vec3 bd = texture(u_backdrop, gl_FragCoord.xy / u_viewPx).rgb;
            float l = dot(bd, vec3(0.2126, 0.7152, 0.0722));
            bd = clamp(mix(vec3(l), bd, 1.8), 0.0, 1.0);
            bd = clamp((bd - 0.5) * 1.05 + 0.5, 0.0, 1.0);
            body = vec4(surf.rgb + bd * (1.0 - surf.a), 1.0);
        } else {
            body = surf;
        }
        // Crowding heat: a flat tint over the surface, under the strip (the
        // HTML card's --heat-bg background layer).
        if (v_heat.a > 0.0) body = over(pm(v_heat), body);
        // background-origin is the padding box: the strip's gradient starts
        // inside the border (3px solid, wash to 64px), under the border is
        // only the surface.
        float sx = p.x - u_borderW;
        float solid = clamp((3.0 - sx) / u_px + 0.5, 0.0, 1.0);
        float washA = 0.13 * u_wash * clamp(1.0 - (sx - 3.0) / 61.0, 0.0, 1.0);
        float inside = clamp(sx / u_px + 0.5, 0.0, 1.0);
        vec4 strip = mix(pm(vec4(v_accent.rgb, washA)), pm(vec4(v_accent.rgb, 1.0)), solid) * inside;
        body = over(strip, body);
        float texMix = v_misc.w * (1.0 - u_greek);
        if (texMix < 1.0) body = over(greek(p) * (1.0 - texMix), body);
        if (texMix > 0.0) {
            vec2 uv = mix(v_uv.xy, v_uv.zw, clamp(p / size, 0.0, 1.0));
            body = over(texture(u_tex, vec3(uv, v_misc.x)) * texMix, body);
        }
        float inBorder = clamp((d + u_borderW) / u_px + 0.5, 0.0, 1.0);
        body = mix(body, over(pm(u_border), body), inBorder);
        col = over(body * cover, col);
    }

    // Handles: 9px dots with a 2px ring of the canvas surface, set 4px past
    // the padding box (inside the 1px border) like the HTML card's.
    for (int i = 0; i < 2; i++) {
        vec2 c = vec2(i == 0 ? u_borderW + 0.5 : size.x - u_borderW - 0.5, size.y * 0.5);
        float dist = length(p - c);
        float hc = clamp((4.5 - dist) / u_px + 0.5, 0.0, 1.0);
        if (hc > 0.0) {
            float inner = clamp((2.5 - dist) / u_px + 0.5, 0.0, 1.0);
            vec4 h = mix(pm(u_handleRing), pm(u_handleFill), inner);
            col = over(h * hc, col);
        }
    }

    if (v_misc.y > 0.5) {
        float l = dot(col.rgb, vec3(0.2126, 0.7152, 0.0722));
        col.rgb = mix(col.rgb, vec3(l), 0.65);
    }
    o = col * v_alpha;
}
`

/** Glass: downsample the backdrop by 4 (bilinear 4-tap = 16 texels). */
export const DOWNSAMPLE_FS = /* glsl */ `${HEADER}
uniform sampler2D u_src;
uniform vec2 u_texel; // source texel size
in vec2 v_uv;
out vec4 o;
void main() {
    vec2 h = u_texel;
    o = 0.25 * (texture(u_src, v_uv + vec2(-h.x, -h.y)) + texture(u_src, v_uv + vec2(h.x, -h.y))
        + texture(u_src, v_uv + vec2(-h.x, h.y)) + texture(u_src, v_uv + vec2(h.x, h.y)));
}
`

/** Glass: one separable gaussian pass. */
export const BLUR_FS = /* glsl */ `${HEADER}
uniform sampler2D u_src;
uniform vec2 u_dir;   // texel step along the pass
uniform float u_sigma; // in texels
in vec2 v_uv;
out vec4 o;
void main() {
    int r = int(min(ceil(u_sigma * 3.0), 32.0));
    vec4 acc = vec4(0.0);
    float wsum = 0.0;
    for (int i = -32; i <= 32; i++) {
        if (i < -r || i > r) continue;
        float fi = float(i);
        float w = exp(-(fi * fi) / (2.0 * u_sigma * u_sigma));
        acc += texture(u_src, v_uv + u_dir * fi) * w;
        wsum += w;
    }
    o = acc / wsum;
}
`

export const COPY_FS = /* glsl */ `${HEADER}
uniform sampler2D u_src;
in vec2 v_uv;
out vec4 o;
void main() { o = texture(u_src, v_uv); }
`
