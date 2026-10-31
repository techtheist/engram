import { textCanvas, type CardLayout } from './cardPainter'
import { FOCUS_RINGS } from './focus'
import type { RGBA, Tokens } from './palette'
import {
    BACKGROUND_FS,
    BLUR_FS,
    CARD_FS,
    CARD_VS,
    COPY_FS,
    DOWNSAMPLE_FS,
    EDGE_FS,
    EDGE_SEGS,
    EDGE_VS,
    FULLSCREEN_VS,
    GLOW_FS,
    GLOW_VS,
    LABEL_FS,
    LABEL_VS,
} from './shaders'

/**
 * The graph canvas on WebGL2 (0.10.1). Vue Flow drew every memory as a
 * live DOM card; at a thousand notes the browser spent its frames laying
 * out text. Here the scene is three instanced draws — edges, edge labels,
 * cards — and the only text work is painting each card once per zoom tier
 * into a texture atlas (cardPainter.ts), visible cards first, a few
 * milliseconds per frame. Below readable size the cards show their text as
 * bars drawn by the shader, so zooming out paints nothing at all. The HTML
 * card (selected, hovered) sits over it.
 */

export interface Camera {
    x: number
    y: number
    zoom: number
}

export interface CardDraw {
    id: string
    x: number
    y: number
    w: number
    h: number
    accent: RGBA
    archived: boolean
    /** Renderer-clock seconds the card first appeared (the spring). */
    appear: number
    layout: CardLayout
    /** Crowding heat (0.11.0): the surface tint, straight rgb + opacity; null = none. */
    heat: RGBA | null
}

/** A crowding halo (0.11.0): the card's rect, the ramp color and its intensity 0..1. */
export interface GlowDraw {
    x: number
    y: number
    w: number
    h: number
    color: [number, number, number]
    intensity: number
}

export interface EdgeDraw {
    id: string
    sx: number
    sy: number
    tx: number
    ty: number
    color: RGBA
    dashed: boolean
    animated: boolean
    label: string | null
    appear: number
}

export interface LabelBox {
    edgeId: string
    x: number
    y: number
    w: number
    h: number
}

export interface GreekMetrics {
    /** Content left edge (border + padding) and top. */
    left: number
    top: number
    headerH: number
    gap: number
    titleLh: number
    bodyLh: number
    contentW: number
}

/** Texture tiers (× dpr). Below the smallest the shader draws text bars. */
const TIERS = [1, 0.5, 0.25]
/**
 * zoom × dpr where card text turns into bars (text is ~4 device px there).
 * A hard switch — a crossfade showed bars and blurry glyphs on top of each
 * other — with a band so hovering at the boundary does not flicker.
 */
const GREEK_ON = 0.28
const GREEK_OFF = 0.32
const LAYER = 2048
const START_LAYERS = 4
const MAX_LAYERS = 12
/** Slot heights round up to this many device px, so slots of one tier share shelves. */
const BUCKET = 32
/** World-unit margin around a card quad: shadow (8 + 3σ of 12) and handles. */
const CARD_MARGIN = 44
const RASTER_BUDGET_MS = 6
/** A failed paint (atlas full) is retried after this many frames, not every frame. */
const RETRY_FRAMES = 30
/** Screen px a frame's slide must exceed to count as fast (see render). */
const FAST_SLIDE_PX = 24
/** A slot not drawn for this many frames (~1 s) is reused before the atlas grows. */
const IDLE_FRAMES = 60
const CARD_FLOATS = 24
const EDGE_FLOATS = 13
const LABEL_FLOATS = 12
const GLOW_FLOATS = 8

/** The tier to paint at: the smallest whose scale still covers the zoom. */
export function tierFor(zoom: number): number {
    for (let i = TIERS.length - 1; i >= 0; i--) if (TIERS[i]! >= zoom) return i
    return 0
}

interface Slot {
    layer: number
    x: number
    y: number
    owner: string | null
    used: number
}

/**
 * A horizontal band of one layer holding slots of one tier and height
 * bucket. Layers are cut into shelves top-down; a shelf that empties goes
 * to whichever tier needs it next. (The first cut dedicated whole layers to
 * a tier and reclaimed them only when empty — after a while of pinch
 * zooming every layer was pinned by a few live cards, nothing could
 * allocate, and cards drew blank.)
 */
interface Shelf {
    layer: number
    y: number
    h: number
    tier: number
    slotW: number
    slots: Slot[]
}

interface Entry {
    slot: Slot
    tier: number
    layout: CardLayout
    w: number
    h: number
    /** Painted a tier small during a fast slide: sharpens once the view slows, whatever the hysteresis says. */
    quick?: boolean
}

function compile(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram {
    const p = gl.createProgram()!
    for (const [type, src] of [
        [gl.VERTEX_SHADER, vs],
        [gl.FRAGMENT_SHADER, fs],
    ] as const) {
        const s = gl.createShader(type)!
        gl.shaderSource(s, src)
        gl.compileShader(s)
        if (!gl.getShaderParameter(s, gl.COMPILE_STATUS) && !gl.isContextLost()) {
            throw new Error(`shader: ${gl.getShaderInfoLog(s)}`)
        }
        gl.attachShader(p, s)
    }
    gl.linkProgram(p)
    if (!gl.getProgramParameter(p, gl.LINK_STATUS) && !gl.isContextLost()) {
        throw new Error(`program: ${gl.getProgramInfoLog(p)}`)
    }
    return p
}

class Program {
    readonly prog: WebGLProgram
    private locs = new Map<string, WebGLUniformLocation | null>()
    constructor(
        private gl: WebGL2RenderingContext,
        vs: string,
        fs: string,
    ) {
        this.prog = compile(gl, vs, fs)
    }
    use(): this {
        this.gl.useProgram(this.prog)
        return this
    }
    loc(name: string): WebGLUniformLocation | null {
        let l = this.locs.get(name)
        if (l === undefined) {
            l = this.gl.getUniformLocation(this.prog, name)
            this.locs.set(name, l)
        }
        return l
    }
    f1(name: string, a: number): this {
        this.gl.uniform1f(this.loc(name), a)
        return this
    }
    f2(name: string, a: number, b: number): this {
        this.gl.uniform2f(this.loc(name), a, b)
        return this
    }
    f3(name: string, a: number, b: number, c: number): this {
        this.gl.uniform3f(this.loc(name), a, b, c)
        return this
    }
    f4(name: string, a: number, b: number, c: number, d: number): this {
        this.gl.uniform4f(this.loc(name), a, b, c, d)
        return this
    }
    i(name: string, v: number): this {
        this.gl.uniform1i(this.loc(name), v)
        return this
    }
    c(name: string, c: RGBA): this {
        return this.f4(name, c[0], c[1], c[2], c[3])
    }
}

interface Target {
    tex: WebGLTexture
    fbo: WebGLFramebuffer
    w: number
    h: number
}

export class GraphRenderer {
    readonly gl: WebGL2RenderingContext
    private bg: Program
    private edgeProg: Program
    private labelProg: Program
    private cardProg: Program
    private downProg: Program
    private blurProg: Program
    private copyProg: Program
    private glowProg: Program
    private emptyVao: WebGLVertexArrayObject
    private quad: WebGLBuffer

    private edgeVao: WebGLVertexArrayObject
    private edgeBuf: WebGLBuffer
    /** Edges uploaded for the current cull box (all of them when all are near). */
    private edgeCount = 0
    private edgeData = new Float32Array(EDGE_FLOATS * 256)
    private edgeTotal = 0
    /** Per edge: its curve's bounding box (x0, y0, x1, y1). */
    private edgeBox = new Float32Array(0)
    private edgeVis = new Float32Array(0)
    /** Per edge: its label's index, or -1. */
    private edgeLabel = new Int32Array(0)
    /** Draw order over the edges (the selection's rings last), or null = as given. */
    private edgeOrder: Uint32Array | null = null
    private labelData = new Float32Array(0)
    private labelVis = new Float32Array(0)
    /**
     * The world box edges and labels were last culled to: the view plus
     * half a view on every side, so a pan re-culls only once it leaves it.
     * At five thousand notes the full set is 7,500 curves of 41 segments —
     * zoomed in, nearly all of that vertex work was off screen.
     */
    private cullBox: [number, number, number, number] | null = null
    private cullDirty = true
    /** Bounding boxes of animated edges (x0, y0, x1, y1 each) — do they need frames? */
    private animBoxes = new Float32Array(0)

    private labelVao: WebGLVertexArrayObject
    private labelBuf: WebGLBuffer
    private labelCount = 0
    private labelTotal = 0
    private labelTex: WebGLTexture | null = null
    private labelUv = new Map<string, { u0: number; v0: number; u1: number; v1: number; w: number; h: number }>()
    labels: LabelBox[] = []

    private cardVao: WebGLVertexArrayObject
    private cardBuf: WebGLBuffer

    private glowVao: WebGLVertexArrayObject
    private glowBuf: WebGLBuffer
    private glowCount = 0
    /** Halo reach past the card's edge, world units (≈ half a card width). */
    glowSpread = 170
    /** Peak opacity of a halo at intensity 1. */
    glowAlpha = 0.09
    private cardData = new Float32Array(CARD_FLOATS * 256)

    private atlas: WebGLTexture | null = null
    private layers = 0
    private shelves: Shelf[] = []
    /** Per layer: the top of its unshelved space. */
    private cursor: number[] = []
    private entries = new Map<string, Entry>()
    /** Frame of a card's last failed paint (atlas full). */
    private failedAt = new Map<string, number>()
    private scratch = new Map<number, { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D }>()
    private frame = 0

    private scene: Target | null = null
    private quarter: Target | null = null
    private blurA: Target | null = null
    private blurB: Target | null = null
    /** The blurred backdrop is stale (camera, edges, size or theme moved). */
    private backdropDirty = true
    private lastCam = { x: NaN, y: NaN, zoom: NaN }
    private greekOn = false

    private tokens: Tokens | null = null
    private cardW = 340
    private cardMaxH = 216
    private radius = 16
    private borderW = 1
    private labelFont = '11px sans-serif'
    private greekM: GreekMetrics = { left: 17, top: 15, headerH: 22, gap: 6, titleLh: 19, bodyLh: 19, contentW: 306 }

    cssW = 1
    cssH = 1
    dpr = 1
    /** A visible card is waiting out a failed paint: ask for a frame later, not every frame. */
    backoff = false
    /** Last frame: visible cards drawn without their texture (as bars). */
    blank = 0
    /** Last frame: an animated edge is on screen. */
    animatedVisible = false

    constructor(readonly canvas: HTMLCanvasElement) {
        const gl = canvas.getContext('webgl2', {
            alpha: false,
            antialias: false,
            premultipliedAlpha: true,
            preserveDrawingBuffer: false,
            failIfMajorPerformanceCaveat: true,
        })
        if (!gl) throw new Error('webgl2 unavailable')
        this.gl = gl
        this.bg = new Program(gl, FULLSCREEN_VS, BACKGROUND_FS)
        this.edgeProg = new Program(gl, EDGE_VS, EDGE_FS)
        this.labelProg = new Program(gl, LABEL_VS, LABEL_FS)
        this.cardProg = new Program(gl, CARD_VS, CARD_FS)
        this.downProg = new Program(gl, FULLSCREEN_VS, DOWNSAMPLE_FS)
        this.blurProg = new Program(gl, FULLSCREEN_VS, BLUR_FS)
        this.copyProg = new Program(gl, FULLSCREEN_VS, COPY_FS)
        this.glowProg = new Program(gl, GLOW_VS, GLOW_FS)
        this.emptyVao = gl.createVertexArray()!

        this.quad = gl.createBuffer()!
        gl.bindBuffer(gl.ARRAY_BUFFER, this.quad)
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW)

        // edges: ends4 color4 style2 focus3 — per instance; vertices come from gl_VertexID
        this.edgeVao = gl.createVertexArray()!
        this.edgeBuf = gl.createBuffer()!
        gl.bindVertexArray(this.edgeVao)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.edgeBuf)
        const es = EDGE_FLOATS * 4
        gl.enableVertexAttribArray(0)
        gl.vertexAttribPointer(0, 4, gl.FLOAT, false, es, 0)
        gl.vertexAttribDivisor(0, 1)
        gl.enableVertexAttribArray(1)
        gl.vertexAttribPointer(1, 4, gl.FLOAT, false, es, 16)
        gl.vertexAttribDivisor(1, 1)
        gl.enableVertexAttribArray(2)
        gl.vertexAttribPointer(2, 2, gl.FLOAT, false, es, 32)
        gl.vertexAttribDivisor(2, 1)
        gl.enableVertexAttribArray(3)
        gl.vertexAttribPointer(3, 3, gl.FLOAT, false, es, 40)
        gl.vertexAttribDivisor(3, 1)

        // labels: box4 uv4 appear1 focus3
        this.labelVao = gl.createVertexArray()!
        this.labelBuf = gl.createBuffer()!
        gl.bindVertexArray(this.labelVao)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.quad)
        gl.enableVertexAttribArray(0)
        gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.labelBuf)
        const ls = LABEL_FLOATS * 4
        gl.enableVertexAttribArray(1)
        gl.vertexAttribPointer(1, 4, gl.FLOAT, false, ls, 0)
        gl.vertexAttribDivisor(1, 1)
        gl.enableVertexAttribArray(2)
        gl.vertexAttribPointer(2, 4, gl.FLOAT, false, ls, 16)
        gl.vertexAttribDivisor(2, 1)
        gl.enableVertexAttribArray(3)
        gl.vertexAttribPointer(3, 1, gl.FLOAT, false, ls, 32)
        gl.vertexAttribDivisor(3, 1)
        gl.enableVertexAttribArray(4)
        gl.vertexAttribPointer(4, 3, gl.FLOAT, false, ls, 36)
        gl.vertexAttribDivisor(4, 1)

        // cards: rect4 accent4 uv4 misc4 parts4 heat4
        this.cardVao = gl.createVertexArray()!
        this.cardBuf = gl.createBuffer()!
        gl.bindVertexArray(this.cardVao)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.quad)
        gl.enableVertexAttribArray(0)
        gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.cardBuf)
        const cs = CARD_FLOATS * 4
        for (let i = 0; i < 6; i++) {
            gl.enableVertexAttribArray(1 + i)
            gl.vertexAttribPointer(1 + i, 4, gl.FLOAT, false, cs, i * 16)
            gl.vertexAttribDivisor(1 + i, 1)
        }

        // glows: rect4 color4
        this.glowVao = gl.createVertexArray()!
        this.glowBuf = gl.createBuffer()!
        gl.bindVertexArray(this.glowVao)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.quad)
        gl.enableVertexAttribArray(0)
        gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.glowBuf)
        for (let i = 0; i < 2; i++) {
            gl.enableVertexAttribArray(1 + i)
            gl.vertexAttribPointer(1 + i, 4, gl.FLOAT, false, GLOW_FLOATS * 4, i * 16)
            gl.vertexAttribDivisor(1 + i, 1)
        }
        gl.bindVertexArray(null)

        gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true)
        // Allocated up front: growing copies every layer, a stall mid-gesture.
        this.growAtlas(START_LAYERS)
    }

    // ---- configuration ----------------------------------------------------

    setSize(cssW: number, cssH: number, dpr: number): void {
        const dprChanged = dpr !== this.dpr
        this.cssW = Math.max(1, cssW)
        this.cssH = Math.max(1, cssH)
        this.dpr = dpr
        this.canvas.width = Math.round(this.cssW * dpr)
        this.canvas.height = Math.round(this.cssH * dpr)
        this.backdropDirty = true
        if (dprChanged) this.resetAtlasLayout()
    }

    setTheme(
        tokens: Tokens,
        card: { width: number; maxH: number; radius: number; border: number; labelFont: string; greek: GreekMetrics },
    ): void {
        this.tokens = tokens
        const sizeChanged = card.width !== this.cardW || card.maxH !== this.cardMaxH
        this.cardW = card.width
        this.cardMaxH = card.maxH
        this.radius = card.radius
        this.borderW = card.border
        this.labelFont = card.labelFont
        this.greekM = card.greek
        this.labelUv.clear()
        for (const sc of this.scratch.values()) sc.canvas.remove()
        this.scratch.clear()
        this.backdropDirty = true
        if (sizeChanged) this.resetAtlasLayout()
        else this.invalidateCards()
    }

    /** Mark every painted card stale (it stays drawable until repainted). */
    invalidateCards(): void {
        for (const e of this.entries.values()) e.layout = null as unknown as CardLayout
    }

    private resetAtlasLayout(): void {
        this.entries.clear()
        this.failedAt.clear()
        this.shelves = []
        this.cursor = this.cursor.map(() => 0)
    }

    forget(id: string): void {
        this.failedAt.delete(id)
        const e = this.entries.get(id)
        if (e) e.slot.owner = null
        this.entries.delete(id)
    }

    // ---- atlas ------------------------------------------------------------

    private scale(tier: number): number {
        return this.dpr * TIERS[tier]!
    }

    private slotW(tier: number): number {
        return Math.ceil(this.cardW * this.scale(tier)) + 1
    }

    /** Device-px slot height for a card `h` tall, rounded up to its bucket. */
    private slotH(tier: number, h: number): number {
        const need = Math.ceil(Math.min(h, this.cardMaxH) * this.scale(tier)) + 1
        return Math.min(LAYER, Math.ceil(need / BUCKET) * BUCKET)
    }

    private growAtlas(n: number): boolean {
        const gl = this.gl
        if (n > MAX_LAYERS || n > gl.getParameter(gl.MAX_ARRAY_TEXTURE_LAYERS)) return false
        const tex = gl.createTexture()!
        gl.bindTexture(gl.TEXTURE_2D_ARRAY, tex)
        gl.texStorage3D(gl.TEXTURE_2D_ARRAY, 1, gl.RGBA8, LAYER, LAYER, n)
        gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MIN_FILTER, gl.LINEAR)
        gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
        gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
        gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
        if (this.atlas) {
            // Carry the painted layers over: read each through a framebuffer.
            const fbo = gl.createFramebuffer()
            gl.bindFramebuffer(gl.READ_FRAMEBUFFER, fbo)
            for (let l = 0; l < this.layers; l++) {
                gl.framebufferTextureLayer(gl.READ_FRAMEBUFFER, gl.COLOR_ATTACHMENT0, this.atlas, 0, l)
                gl.copyTexSubImage3D(gl.TEXTURE_2D_ARRAY, 0, 0, 0, l, 0, 0, LAYER, LAYER)
            }
            gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null)
            gl.deleteFramebuffer(fbo)
            gl.deleteTexture(this.atlas)
        }
        this.atlas = tex
        for (let l = this.layers; l < n; l++) this.cursor.push(0)
        this.layers = n
        return true
    }

    /** (Re)cut a shelf for `tier`; any owners left on it lose their textures. */
    private cutShelf(shelf: Shelf, tier: number): Slot {
        for (const sl of shelf.slots) if (sl.owner) this.entries.delete(sl.owner)
        const w = this.slotW(tier)
        shelf.tier = tier
        shelf.slotW = w
        shelf.slots = []
        for (let x = 0; x + w <= LAYER; x += w) shelf.slots.push({ layer: shelf.layer, x, y: shelf.y, owner: null, used: 0 })
        return shelf.slots[0]!
    }

    private alloc(tier: number, h: number): Slot | null {
        // 1. A free slot on a shelf of this tier and height.
        for (const sh of this.shelves) {
            if (sh.tier !== tier || sh.h !== h) continue
            for (const sl of sh.slots) if (!sl.owner) return sl
        }
        // 2. An empty shelf of any tier, the tightest that fits.
        let empty: Shelf | null = null
        for (const sh of this.shelves) {
            if (sh.h < h || (empty && sh.h >= empty.h)) continue
            if (sh.slots.every((sl) => !sl.owner)) empty = sh
        }
        if (empty) return this.cutShelf(empty, tier)
        // 3. Unshelved space in a layer.
        const fresh = this.unshelved(tier, h)
        if (fresh) return fresh
        // 4. Evict the least recently drawn slot of this tier and height — first
        //    only one idle for a while, then (atlas full) any not on screen;
        //    visible cards were stamped before painting began. Reuse comes
        //    before growth: growing copies every layer (a stall), and a long
        //    session on a big graph crept to the 12-layer cap (~200 MB).
        const victimOf = (before: number): Slot | null => {
            let victim: Slot | null = null
            for (const sh of this.shelves) {
                if (sh.tier !== tier || sh.h !== h) continue
                for (const sl of sh.slots) if (sl.used < before && (!victim || sl.used < victim.used)) victim = sl
            }
            return victim
        }
        let victim = victimOf(this.frame - IDLE_FRAMES)
        if (!victim && this.growAtlas(Math.min(MAX_LAYERS, this.layers * 2))) {
            const grown = this.unshelved(tier, h)
            if (grown) return grown
        }
        victim ??= victimOf(this.frame)
        if (victim) {
            if (victim.owner) this.entries.delete(victim.owner)
            victim.owner = null
            return victim
        }
        // 5. Re-cut the stalest tall-enough shelf with nothing on screen.
        let stale: Shelf | null = null
        let staleUse = Infinity
        for (const sh of this.shelves) {
            if (sh.h < h) continue
            let use = 0
            for (const sl of sh.slots) if (sl.used > use) use = sl.used
            if (use < this.frame && use < staleUse) {
                stale = sh
                staleUse = use
            }
        }
        if (stale) return this.cutShelf(stale, tier)
        // 6. Nothing tall enough to re-cut: the atlas is full of a smaller
        //    tier's short shelves (a fly-over at low zoom paints hundreds).
        //    Clear the stalest whole layer with no card on screen.
        return this.reclaimLayer() ? this.unshelved(tier, h) : null
    }

    /** Empty the least recently drawn layer that has nothing on screen. */
    private reclaimLayer(): boolean {
        const last = new Array<number>(this.layers).fill(0)
        for (const sh of this.shelves) for (const sl of sh.slots) if (sl.used > last[sh.layer]!) last[sh.layer] = sl.used
        let pick = -1
        for (let l = 0; l < this.layers; l++) if (last[l]! < this.frame && (pick < 0 || last[l]! < last[pick]!)) pick = l
        if (pick < 0) return false
        this.shelves = this.shelves.filter((sh) => {
            if (sh.layer !== pick) return true
            for (const sl of sh.slots) if (sl.owner) this.entries.delete(sl.owner)
            return false
        })
        this.cursor[pick] = 0
        return true
    }

    /** A new shelf in a layer's unshelved space, if any layer has room. */
    private unshelved(tier: number, h: number): Slot | null {
        for (let l = 0; l < this.layers; l++) {
            if (this.cursor[l]! + h > LAYER) continue
            const sh: Shelf = { layer: l, y: this.cursor[l]!, h, tier, slotW: 0, slots: [] }
            this.cursor[l] = this.cursor[l]! + h
            this.shelves.push(sh)
            return this.cutShelf(sh, tier)
        }
        return null
    }

    /** Paint `card` at `want`, or a smaller tier when the atlas is full. */
    private paint(card: CardDraw, want: number): boolean {
        const gl = this.gl
        const old = this.entries.get(card.id)
        let tier = want
        let slot: Slot | null = null
        for (; tier < TIERS.length; tier++) {
            // A card that has a texture repaints only for a sharper one (or
            // the exact smaller tier it was asked down to). Falling back to
            // the tier it already has evicted another card for nothing, and
            // with the atlas full of small slots at 100% zoom that ran every
            // frame — ~30 repaints and uploads a frame, 80–200 ms frames.
            if (old && old.layout === card.layout && tier >= old.tier && tier !== want) break
            slot = this.alloc(tier, this.slotH(tier, card.h))
            if (slot) break
        }
        if (!slot) {
            this.failedAt.set(card.id, this.frame)
            return false
        }
        this.failedAt.delete(card.id)
        const w = this.slotW(tier)
        const h = this.slotH(tier, card.h)
        // One scratch canvas per tier and height bucket: exactly the slot's
        // size, so the canvas uploads as is and never resizes between cards.
        const key = tier * 4096 + h
        let sc = this.scratch.get(key)
        if (!sc) {
            const canvas = textCanvas(w, h)
            // CPU-backed on purpose: uploading a GPU canvas into a texture is
            // a readback per card (4 s of a thousand-card load); a software
            // canvas uploads as a plain copy, and draws the same glyphs.
            sc = { canvas, ctx: canvas.getContext('2d', { willReadFrequently: true })! }
            this.scratch.set(key, sc)
        }
        const s = this.scale(tier)
        const ctx = sc.ctx
        ctx.setTransform(1, 0, 0, 1, 0, 0)
        ctx.clearRect(0, 0, w, h)
        ctx.setTransform(s, 0, 0, s, 0, 0)
        card.layout.paint(ctx)
        gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.atlas)
        gl.texSubImage3D(gl.TEXTURE_2D_ARRAY, 0, slot.x, slot.y, slot.layer, w, h, 1, gl.RGBA, gl.UNSIGNED_BYTE, sc.canvas)
        // The old slot frees only once its replacement holds the card.
        if (old && old.slot !== slot) old.slot.owner = null
        slot.owner = card.id
        slot.used = this.frame
        this.entries.set(card.id, { slot, tier, layout: card.layout, w: card.w, h: card.h })
        return true
    }

    // ---- crowding glow -------------------------------------------------------

    /** The heat halos, drawn in the given order (hottest last reads best). Empty = off. */
    setGlows(glows: GlowDraw[]): void {
        const d = new Float32Array(glows.length * GLOW_FLOATS)
        let o = 0
        for (const g of glows) {
            d[o] = g.x
            d[o + 1] = g.y
            d[o + 2] = g.w
            d[o + 3] = g.h
            d[o + 4] = g.color[0]
            d[o + 5] = g.color[1]
            d[o + 6] = g.color[2]
            d[o + 7] = g.intensity
            o += GLOW_FLOATS
        }
        const gl = this.gl
        gl.bindVertexArray(null)
        gl.bindBuffer(gl.ARRAY_BUFFER, this.glowBuf)
        gl.bufferData(gl.ARRAY_BUFFER, d, gl.DYNAMIC_DRAW)
        this.glowCount = glows.length
        this.backdropDirty = true
    }

    /** Tuning knobs: reach (world units) and peak opacity. */
    setGlowStyle(spread: number, alpha: number): void {
        this.glowSpread = spread
        this.glowAlpha = alpha
        this.backdropDirty = true
    }

    // ---- edges & labels -----------------------------------------------------

    setEdges(edges: EdgeDraw[]): void {
        if (this.edgeData.length < edges.length * EDGE_FLOATS) this.edgeData = new Float32Array(Math.ceil(edges.length * 1.5) * EDGE_FLOATS)
        const d = this.edgeData
        const labels: LabelBox[] = []
        const labelData: number[] = []
        const anim: number[] = []
        const boxes: number[] = []
        const edgeLabel: number[] = []
        const off = (dx: number): number => (dx >= 0 ? 0.5 * dx : 6.25 * Math.sqrt(-dx))
        // Every verb into the atlas before any uv is read: a rebuild changes
        // the atlas height, and with it every v already handed out — built
        // mid-loop, the earlier edges sampled a squashed slice of two labels.
        const fresh = new Set<string>()
        for (const e of edges) if (e.label && !this.labelUv.has(e.label)) fresh.add(e.label)
        if (fresh.size) this.buildLabelAtlas([...this.labelUv.keys(), ...fresh])
        let o = 0
        for (const e of edges) {
            d[o] = e.sx
            d[o + 1] = e.sy
            d[o + 2] = e.tx
            d[o + 3] = e.ty
            d[o + 4] = e.color[0]
            d[o + 5] = e.color[1]
            d[o + 6] = e.color[2]
            d[o + 7] = e.color[3]
            d[o + 8] = (e.dashed ? 1 : 0) | (e.animated ? 2 : 0)
            d[o + 9] = e.appear
            d[o + 10] = d[o + 11] = d[o + 12] = 0
            o += EDGE_FLOATS
            const k = off(e.tx - e.sx)
            const c1x = e.sx + k
            const c2x = e.tx - k
            // The curve stays inside its control points' box; its y only
            // spans the ends (the controls are level with them).
            const bx0 = Math.min(e.sx, e.tx, c1x, c2x)
            const by0 = Math.min(e.sy, e.ty)
            const bx1 = Math.max(e.sx, e.tx, c1x, c2x)
            const by1 = Math.max(e.sy, e.ty)
            boxes.push(bx0, by0, bx1, by1)
            if (e.animated) anim.push(bx0, by0, bx1, by1)
            if (e.label) {
                const lu = this.labelUv.get(e.label)!
                const cx = e.sx * 0.125 + c1x * 0.375 + c2x * 0.375 + e.tx * 0.125
                const cy = e.sy * 0.5 + e.ty * 0.5
                labels.push({ edgeId: e.id, x: cx - lu.w / 2, y: cy - lu.h / 2, w: lu.w, h: lu.h })
                edgeLabel.push(labels.length - 1)
                labelData.push(cx, cy, lu.w, lu.h, lu.u0, lu.v0, lu.u1, lu.v1, e.appear, 0, 0, 0)
            } else {
                edgeLabel.push(-1)
            }
        }
        this.edgeTotal = edges.length
        this.edgeLabel = Int32Array.from(edgeLabel)
        this.edgeOrder = null
        this.edgeBox = new Float32Array(boxes)
        this.animBoxes = new Float32Array(anim)
        this.labelData = new Float32Array(labelData)
        this.labelTotal = labels.length
        this.labels = labels
        // Uploaded by the next frame's cull.
        this.cullDirty = true
        this.backdropDirty = true
    }

    /**
     * Selection focus (focus.ts), three floats per edge in `setEdges` order:
     * previous focus, focus, since (renderer clock) — written in place, so a
     * new selection costs no rebuild. Edges draw in instance order, so the
     * selection's rings are moved to the end, ring 1 last (on top).
     */
    setEdgeFocus(focus: Float32Array): void {
        const n = this.edgeTotal
        const d = this.edgeData
        const ld = this.labelData
        const buckets: number[][] = Array.from({ length: FOCUS_RINGS + 1 }, () => [])
        for (let i = 0; i < n; i++) {
            const from = focus[i * 3]!
            const to = focus[i * 3 + 1]!
            const at = focus[i * 3 + 2]!
            const o = i * EDGE_FLOATS + 10
            d[o] = from
            d[o + 1] = to
            d[o + 2] = at
            const l = this.edgeLabel[i]!
            if (l >= 0) {
                const lo = l * LABEL_FLOATS + 9
                ld[lo] = from
                ld[lo + 1] = to
                ld[lo + 2] = at
            }
            buckets[to > 0 ? FOCUS_RINGS + 1 - to : 0]!.push(i)
        }
        this.edgeOrder = buckets[0]!.length === n ? null : Uint32Array.from(buckets.flat())
        this.cullDirty = true
        this.backdropDirty = true
    }

    /** Upload the edges and labels near the view (see `cullBox`). */
    private cull(x0: number, y0: number, x1: number, y1: number): void {
        const c = this.cullBox
        const inside = c !== null && x0 >= c[0] && y0 >= c[1] && x1 <= c[2] && y1 <= c[3]
        // Zoomed far in since the last cull: a tighter box drops more.
        const loose = c !== null && c[2] - c[0] > 4 * (x1 - x0)
        if (!this.cullDirty && inside && !loose) return
        const mx = (x1 - x0) / 2
        const my = (y1 - y0) / 2
        const b: [number, number, number, number] = [x0 - mx, y0 - my, x1 + mx, y1 + my]
        this.cullBox = b
        this.cullDirty = false
        const gl = this.gl
        gl.bindVertexArray(null)

        const n = this.edgeTotal
        const box = this.edgeBox
        const all = this.edgeData
        if (this.edgeVis.length < n * EDGE_FLOATS) this.edgeVis = new Float32Array(all.length)
        const vis = this.edgeVis
        const order = this.edgeOrder
        let count = 0
        for (let j = 0; j < n; j++) {
            const i = order ? order[j]! : j
            const k = i * 4
            if (box[k]! > b[2] || box[k + 2]! < b[0] || box[k + 1]! > b[3] || box[k + 3]! < b[1]) continue
            const from = i * EDGE_FLOATS
            const to = count * EDGE_FLOATS
            for (let f = 0; f < EDGE_FLOATS; f++) vis[to + f] = all[from + f]!
            count++
        }
        gl.bindBuffer(gl.ARRAY_BUFFER, this.edgeBuf)
        gl.bufferData(gl.ARRAY_BUFFER, (count === n && !order ? all : vis).subarray(0, count * EDGE_FLOATS), gl.DYNAMIC_DRAW)
        this.edgeCount = count

        const lab = this.labels
        const ld = this.labelData
        if (this.labelVis.length < ld.length) this.labelVis = new Float32Array(ld.length)
        const lv = this.labelVis
        let lc = 0
        for (let i = 0; i < this.labelTotal; i++) {
            const l = lab[i]!
            if (l.x > b[2] || l.x + l.w < b[0] || l.y > b[3] || l.y + l.h < b[1]) continue
            for (let f = 0; f < LABEL_FLOATS; f++) lv[lc * LABEL_FLOATS + f] = ld[i * LABEL_FLOATS + f]!
            lc++
        }
        gl.bindBuffer(gl.ARRAY_BUFFER, this.labelBuf)
        gl.bufferData(gl.ARRAY_BUFFER, (lc === this.labelTotal ? ld : lv).subarray(0, lc * LABEL_FLOATS), gl.DYNAMIC_DRAW)
        this.labelCount = lc
    }

    /** Edge labels: the verb in a soft box (EngramEdge's look), one atlas for all. */
    private buildLabelAtlas(labels: string[]): void {
        const gl = this.gl
        const t = this.tokens
        const canvas = textCanvas(1, 1)
        const ctx = canvas.getContext('2d')!
        const s = Math.min(2, this.dpr) * 2
        ctx.font = this.labelFont
        const m = ctx.measureText('Hg')
        const textH = m.fontBoundingBoxAscent + m.fontBoundingBoxDescent
        const boxes = labels.map((l) => ({ l, w: ctx.measureText(l).width + 8, h: textH + 4 }))
        const W = 1024
        let x = 0
        let y = 0
        let rowH = 0
        const spots = boxes.map((b) => {
            if (x + b.w * s + 2 > W) {
                x = 0
                y += rowH + 2
                rowH = 0
            }
            const spot = { ...b, x, y }
            x += Math.ceil(b.w * s) + 2
            rowH = Math.max(rowH, Math.ceil(b.h * s))
            return spot
        })
        canvas.width = W
        canvas.height = Math.max(4, y + rowH)
        ctx.font = this.labelFont
        const bg = t ? t.color['surface-base'] : ([0, 0, 0, 1] as RGBA)
        const fg = t ? t.color['text-tertiary'] : ([0.5, 0.5, 0.5, 1] as RGBA)
        this.labelUv.clear()
        for (const b of spots) {
            ctx.setTransform(s, 0, 0, s, b.x, b.y)
            ctx.beginPath()
            ctx.roundRect(0, 0, b.w, b.h, 2)
            ctx.fillStyle = `rgba(${bg[0] * 255}, ${bg[1] * 255}, ${bg[2] * 255}, 0.85)`
            ctx.fill()
            ctx.fillStyle = `rgba(${fg[0] * 255}, ${fg[1] * 255}, ${fg[2] * 255}, ${fg[3]})`
            ctx.textBaseline = 'alphabetic'
            ctx.fillText(b.l, 4, 2 + m.fontBoundingBoxAscent)
            this.labelUv.set(b.l, {
                u0: b.x / W,
                v0: b.y / canvas.height,
                u1: (b.x + b.w * s) / W,
                v1: (b.y + b.h * s) / canvas.height,
                w: b.w,
                h: b.h,
            })
        }
        canvas.remove()
        if (this.labelTex) gl.deleteTexture(this.labelTex)
        this.labelTex = gl.createTexture()!
        gl.bindTexture(gl.TEXTURE_2D, this.labelTex)
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, canvas)
        gl.generateMipmap(gl.TEXTURE_2D)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
    }

    // ---- frame ------------------------------------------------------------

    private target(t: Target | null, w: number, h: number): Target {
        const gl = this.gl
        if (t && t.w === w && t.h === h) return t
        if (t) {
            gl.deleteTexture(t.tex)
            gl.deleteFramebuffer(t.fbo)
        }
        const tex = gl.createTexture()!
        gl.bindTexture(gl.TEXTURE_2D, tex)
        gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, w, h)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
        const fbo = gl.createFramebuffer()!
        gl.bindFramebuffer(gl.FRAMEBUFFER, fbo)
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0)
        return { tex, fbo, w, h }
    }

    private fullscreen(p: Program, src: WebGLTexture | null): void {
        const gl = this.gl
        if (src) {
            gl.activeTexture(gl.TEXTURE0)
            gl.bindTexture(gl.TEXTURE_2D, src)
            p.i('u_src', 0)
        }
        gl.bindVertexArray(this.emptyVao)
        gl.drawArrays(gl.TRIANGLES, 0, 3)
    }

    /** Background, edges and labels — what lies under the cards. */
    private drawBackdrop(cam: Camera, time: number, W: number, H: number): void {
        const gl = this.gl
        const t = this.tokens!
        const c = t.color
        const z = cam.zoom
        gl.disable(gl.BLEND)
        this.bg
            .use()
            .c('u_bg', c['canvas-bg'])
            .c('u_dots', c['canvas-dots'])
            .c('u_glow1', c['canvas-glow-1'])
            .c('u_glow2', c['canvas-glow-2'])
            .f3('u_cam', cam.x, cam.y, z)
            .f1('u_dpr', this.dpr)
            .f2('u_viewPx', W, H)
            .f1('u_rem', t.rem)
        this.fullscreen(this.bg, null)
        gl.enable(gl.BLEND)
        if (this.glowCount) {
            // Under the edges and the cards; glass cards pick it up through the blur.
            this.glowProg
                .use()
                .f3('u_cam', cam.x, cam.y, z)
                .f2('u_view', this.cssW, this.cssH)
                .f1('u_spread', this.glowSpread)
                .f1('u_alpha', this.glowAlpha)
                .f1('u_radius', this.radius)
            gl.bindVertexArray(this.glowVao)
            gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, this.glowCount)
        }
        if (this.edgeCount) {
            // Fewer segments as edges shrink on screen: zoomed out, 40 per
            // curve was most of the frame's vertex work for no visible gain.
            const segs = z >= 0.45 ? EDGE_SEGS : z >= 0.2 ? 24 : 16
            this.edgeProg
                .use()
                .f3('u_cam', cam.x, cam.y, z)
                .f2('u_view', this.cssW, this.cssH)
                .f1('u_dpr', this.dpr)
                .f1('u_width', 2)
                .f1('u_time', time)
                .f1('u_segs', segs)
                .f1('u_static', this.greekOn ? 1 : 0)
            gl.bindVertexArray(this.edgeVao)
            gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, (segs + 1) * 2, this.edgeCount)
        }
        // Verb labels are specks at bar zoom: not drawn there.
        if (this.labelCount && this.labelTex && !this.greekOn) {
            this.labelProg.use().f3('u_cam', cam.x, cam.y, z).f2('u_view', this.cssW, this.cssH).f1('u_time', time)
            gl.activeTexture(gl.TEXTURE0)
            gl.bindTexture(gl.TEXTURE_2D, this.labelTex)
            this.labelProg.i('u_tex', 0)
            gl.bindVertexArray(this.labelVao)
            gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, this.labelCount)
        }
    }

    /**
     * Draw one frame. `settled`: the zoom stopped moving, so cards may be
     * repainted at its tier; otherwise only cards with no texture are.
     * `backdropLive`: the blurred backdrop must follow this frame (an edge
     * appear animation); dash motion alone never re-blurs it.
     * Returns true when more painting is queued (call again next frame).
     */
    render(cam: Camera, time: number, cards: CardDraw[], skip: Set<string>, settled: boolean, backdropLive = false): boolean {
        const gl = this.gl
        const t = this.tokens
        if (!t || gl.isContextLost()) return false
        this.frame++
        const frame = this.frame
        const W = this.canvas.width
        const H = this.canvas.height
        const c = t.color
        const z = cam.zoom
        const zd = z * this.dpr
        // Screen px the view slid since the last frame (pans only: a zoom
        // has its own settle rule).
        let slid = 0
        if (cam.x !== this.lastCam.x || cam.y !== this.lastCam.y || z !== this.lastCam.zoom) {
            this.backdropDirty = true
            if (z === this.lastCam.zoom) slid = Math.hypot(cam.x - this.lastCam.x, cam.y - this.lastCam.y)
            this.lastCam = { x: cam.x, y: cam.y, zoom: z }
        }
        /*
         * A fast slide (a minimap fly-over, a fling) brings new cards every
         * frame, and a full-size upload is 15–25 ms at dpr 2 — the flight
         * stuttered on texSubImage3D. While it lasts, new cards paint at half
         * size (a quarter of the bytes) and sharpening waits; once the view
         * slows, the settled rule repaints them sharp, nearest first. Drags
         * and wheel pans stay under the threshold.
         */
        const fast = slid > FAST_SLIDE_PX

        // Visible cards — stamped as used before any painting, so painting
        // never evicts a card that is on screen.
        const vx0 = -cam.x / z - CARD_MARGIN
        const vy0 = -cam.y / z - CARD_MARGIN
        const vx1 = (this.cssW - cam.x) / z + CARD_MARGIN
        const vy1 = (this.cssH - cam.y) / z + CARD_MARGIN
        const visible: CardDraw[] = []
        for (const card of cards) {
            if (skip.has(card.id)) continue
            if (card.x > vx1 || card.y > vy1 || card.x + card.w < vx0 || card.y + card.h < vy0) continue
            visible.push(card)
            const e = this.entries.get(card.id)
            if (e) e.slot.used = frame
        }

        // Painting. Below readable size there is none: the shader's bars stand in.
        if (this.greekOn && zd > GREEK_OFF) this.greekOn = false
        else if (!this.greekOn && zd < GREEK_ON) this.greekOn = true
        const greek = this.greekOn ? 1 : 0

        // Dashes only move while text is readable: zoomed out to bars, every
        // edge is a still line, and nothing asks for frames.
        let animatedVisible = false
        const ab = this.animBoxes
        for (let i = 0; i < ab.length && !greek; i += 4) {
            if (ab[i]! <= vx1 && ab[i + 2]! >= vx0 && ab[i + 1]! <= vy1 && ab[i + 3]! >= vy0) {
                animatedVisible = true
                break
            }
        }
        this.animatedVisible = animatedVisible
        this.cull(vx0, vy0, vx1, vy1)
        let more = false
        let backoff = false
        if (greek < 1) {
            const want = tierFor(z)
            const quick = fast ? Math.min(TIERS.length - 1, want + 1) : want
            const todo: CardDraw[] = []
            const urgent: CardDraw[] = []
            for (const card of visible) {
                const failed = this.failedAt.get(card.id)
                if (failed !== undefined && frame - failed < RETRY_FRAMES) {
                    backoff = true
                    continue
                }
                const e = this.entries.get(card.id)
                if (!e) {
                    urgent.push(card)
                    continue
                }
                if (e.layout !== card.layout) {
                    todo.push(card)
                    continue
                }
                // Hysteresis: a texture is kept while its scale is within
                // [0.75, 2.2]× the zoom; outside that it repaints once the
                // zoom has settled — up when blurry, down to free memory.
                const ratio = TIERS[e.tier]! / z
                if (settled && !fast && (e.quick || ratio < 0.75 || ratio > 2.2) && e.tier !== want) todo.push(card)
            }
            if (urgent.length + todo.length) {
                const cx = (this.cssW / 2 - cam.x) / z
                const cy = (this.cssH / 2 - cam.y) / z
                const dist = (k: CardDraw): number => Math.abs(k.x + k.w / 2 - cx) + Math.abs(k.y + k.h / 2 - cy)
                urgent.sort((a, b) => dist(a) - dist(b))
                todo.sort((a, b) => dist(a) - dist(b))
                const t0 = performance.now()
                let done = 0
                for (const list of [urgent, todo]) {
                    for (const card of list) {
                        if (done > 0 && performance.now() - t0 > RASTER_BUDGET_MS) {
                            more = true
                            break
                        }
                        const tier = list === urgent ? quick : want
                        if (this.paint(card, tier) && tier !== want) this.entries.get(card.id)!.quick = true
                        done++
                    }
                    if (more) break
                }
            }
        }
        // A fast frame deferred sharpening: one more frame, so the view that
        // just stopped gets its full-size repaints without another input.
        if (fast) more = true
        this.backoff = backoff

        // Card instances.
        if (this.cardData.length < visible.length * CARD_FLOATS) this.cardData = new Float32Array(Math.ceil(visible.length * 1.5) * CARD_FLOATS)
        const d = this.cardData
        let o = 0
        let blank = 0
        for (const card of visible) {
            const e = this.entries.get(card.id)
            d[o] = card.x
            d[o + 1] = card.y
            d[o + 2] = card.w
            d[o + 3] = card.h
            d[o + 4] = card.accent[0]
            d[o + 5] = card.accent[1]
            d[o + 6] = card.accent[2]
            d[o + 7] = card.accent[3]
            if (e) {
                const s = this.scale(e.tier)
                d[o + 8] = e.slot.x / LAYER
                d[o + 9] = e.slot.y / LAYER
                d[o + 10] = (e.slot.x + e.w * s) / LAYER
                d[o + 11] = (e.slot.y + e.h * s) / LAYER
                d[o + 12] = e.slot.layer
            } else {
                d[o + 8] = d[o + 9] = d[o + 10] = d[o + 11] = d[o + 12] = 0
                if (greek < 1) blank++
            }
            d[o + 13] = card.archived ? 1 : 0
            d[o + 14] = card.appear
            d[o + 15] = e ? 1 : 0
            const parts = card.layout.parts
            d[o + 16] = parts.title
            d[o + 17] = parts.body
            d[o + 18] = parts.refs
            d[o + 19] = parts.tags
            const heat = card.heat
            d[o + 20] = heat ? heat[0] : 0
            d[o + 21] = heat ? heat[1] : 0
            d[o + 22] = heat ? heat[2] : 0
            d[o + 23] = heat ? heat[3] : 0
            o += CARD_FLOATS
        }
        this.blank = blank
        const n = visible.length

        // Backdrop: with glass, re-blurred only when it moved; drawn straight
        // to the screen otherwise.
        gl.enable(gl.BLEND)
        gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA)
        const glass = t.glass
        let backdrop: WebGLTexture | null = null
        if (glass && (this.backdropDirty || backdropLive || !this.blurA)) {
            this.scene = this.target(this.scene, W, H)
            gl.bindFramebuffer(gl.FRAMEBUFFER, this.scene.fbo)
            gl.viewport(0, 0, W, H)
            this.drawBackdrop(cam, time, W, H)
            // ÷4 then ÷8, then a separable gaussian at 1/8 resolution.
            const qw = Math.max(1, Math.round(W / 4))
            const qh = Math.max(1, Math.round(H / 4))
            const ew = Math.max(1, Math.round(W / 8))
            const eh = Math.max(1, Math.round(H / 8))
            this.quarter = this.target(this.quarter, qw, qh)
            this.blurA = this.target(this.blurA, ew, eh)
            this.blurB = this.target(this.blurB, ew, eh)
            gl.disable(gl.BLEND)
            gl.viewport(0, 0, qw, qh)
            gl.bindFramebuffer(gl.FRAMEBUFFER, this.quarter.fbo)
            this.downProg.use().f2('u_texel', 1 / W, 1 / H)
            this.fullscreen(this.downProg, this.scene.tex)
            gl.viewport(0, 0, ew, eh)
            gl.bindFramebuffer(gl.FRAMEBUFFER, this.blurA.fbo)
            this.downProg.f2('u_texel', 1 / qw, 1 / qh)
            this.fullscreen(this.downProg, this.quarter.tex)
            const sigma = (14 * this.dpr) / 8
            gl.bindFramebuffer(gl.FRAMEBUFFER, this.blurB.fbo)
            this.blurProg.use().f2('u_dir', 1 / ew, 0).f1('u_sigma', sigma)
            this.fullscreen(this.blurProg, this.blurA.tex)
            gl.bindFramebuffer(gl.FRAMEBUFFER, this.blurA.fbo)
            this.blurProg.f2('u_dir', 0, 1 / eh)
            this.fullscreen(this.blurProg, this.blurB.tex)
            this.backdropDirty = false
            gl.bindFramebuffer(gl.FRAMEBUFFER, null)
            gl.viewport(0, 0, W, H)
            this.copyProg.use()
            this.fullscreen(this.copyProg, this.scene.tex)
            gl.enable(gl.BLEND)
        } else {
            gl.bindFramebuffer(gl.FRAMEBUFFER, null)
            gl.viewport(0, 0, W, H)
            this.drawBackdrop(cam, time, W, H)
        }
        if (glass) backdrop = this.blurA?.tex ?? null

        // Cards.
        if (n) {
            gl.bindBuffer(gl.ARRAY_BUFFER, this.cardBuf)
            gl.bufferData(gl.ARRAY_BUFFER, d.subarray(0, o), gl.STREAM_DRAW)
            const se = c['surface-elevated']
            const g = this.greekM
            const p = this.cardProg
            p.use()
                .f3('u_cam', cam.x, cam.y, z)
                .f2('u_view', this.cssW, this.cssH)
                .f1('u_time', time)
                .f1('u_margin', CARD_MARGIN)
                .f4('u_surface', se[0], se[1], se[2], glass ? 0.8 : se[3])
                .c('u_border', c['border-default'])
                .c('u_handleFill', c['border-strong'])
                .c('u_handleRing', c['surface-base'])
                .f1('u_radius', this.radius)
                .f1('u_borderW', this.borderW)
                .f1('u_wash', t.wash)
                .f1('u_px', 1 / zd)
                .f1('u_glass', glass ? 1 : 0)
                .f2('u_viewPx', W, H)
                // The shadow's blur is under a device pixel: skip its math.
                .f1('u_shadow', 12 * zd >= 1 ? 1 : 0)
                .f1('u_greek', greek)
                .f4('u_gk0', g.left, g.top, g.headerH, g.gap)
                .f4('u_gk1', g.titleLh, g.bodyLh, g.contentW, 0)
                .c('u_textPrimary', c['text-primary'])
                .c('u_textSecondary', c['text-secondary'])
                .c('u_textTertiary', c['text-tertiary'])
                .c('u_primary', c['interactive-primary'])
            gl.activeTexture(gl.TEXTURE0)
            gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.atlas)
            p.i('u_tex', 0)
            gl.activeTexture(gl.TEXTURE1)
            gl.bindTexture(gl.TEXTURE_2D, backdrop)
            p.i('u_backdrop', 1)
            gl.bindVertexArray(this.cardVao)
            gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, n)
            gl.activeTexture(gl.TEXTURE0)
        }
        gl.bindVertexArray(null)
        return more
    }

    atlasInfo(): { layers: number; shelves: number; entries: number } {
        return { layers: this.layers, shelves: this.shelves.length, entries: this.entries.size }
    }

    dispose(): void {
        for (const sc of this.scratch.values()) sc.canvas.remove()
        this.scratch.clear()
        this.gl.getExtension('WEBGL_lose_context')?.loseContext()
    }
}
