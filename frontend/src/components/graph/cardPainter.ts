import MarkdownIt from 'markdown-it'
import type Token from 'markdown-it/lib/token.mjs'
import type { GraphNode } from '@/types/graph'
import { css, fade, type CardMetrics, type RGBA, type Tokens } from './palette'

/**
 * The collapsed card, laid out and painted with Canvas2D (0.10.1) — the
 * browser's own text engine, so glyphs match the HTML card pixel for pixel
 * in every font the pane can be handed (the IDE's included). Layout
 * follows EngramCard's CSS: a flex column capped at its max height where
 * only the body shrinks, a five-line title clamp, one row of chips, the
 * body clipped to its 9rem. The shape (surface, border, accent strip,
 * shadow, handles) is the shader's; this paints only what sits on it.
 */

export interface CardLayout {
    /** Border-box height, px. */
    h: number
    /** Section heights as laid out — for checking against the HTML card. */
    parts: { header: number; title: number; body: number; refs: number; tags: number }
    paint(ctx: CanvasRenderingContext2D): void
}

// ---- text measuring ---------------------------------------------------------

/**
 * Every canvas that measures or paints card text is attached under one host
 * (GraphGL's hidden probe) with one style. Canvas text takes font smoothing
 * and font-variant from the element's computed style: a detached canvas
 * draws heavier macOS glyphs, and one that inherits the page's tabular
 * figures widens hyphens and digits — measured on one canvas and painted on
 * another, words then overlap ("continuous - scrollreading"). Proportional
 * figures, like the HTML card (EngramCard sets them too).
 */
const TEXT_CANVAS_CSS =
    'position:absolute;left:0;top:0;width:1px;height:1px;font-variant-numeric:normal;font-feature-settings:normal;'
let textHost: HTMLElement | null = null

export function textCanvas(w: number, h: number): HTMLCanvasElement {
    const canvas = document.createElement('canvas')
    canvas.width = w
    canvas.height = h
    canvas.style.cssText = TEXT_CANVAS_CSS
    textHost?.appendChild(canvas)
    return canvas
}

let measureCtx: CanvasRenderingContext2D | null = null
const widths = new Map<string, number>()
const vmetrics = new Map<string, { asc: number; desc: number }>()

/** Where text canvases live; measurements restart under the new host. */
export function setTextHost(host: HTMLElement | null): void {
    textHost = host
    measureCtx?.canvas.remove()
    measureCtx = null
    resetMeasures()
}

function mctx(): CanvasRenderingContext2D {
    if (!measureCtx) measureCtx = textCanvas(1, 1).getContext('2d')!
    return measureCtx
}

function measure(font: string, text: string): number {
    const key = font + '\u0000' + text
    let w = widths.get(key)
    if (w === undefined) {
        const c = mctx()
        c.font = font
        w = c.measureText(text).width
        if (widths.size > 60_000) widths.clear()
        widths.set(key, w)
    }
    return w
}

function vmetric(font: string): { asc: number; desc: number } {
    let m = vmetrics.get(font)
    if (!m) {
        const c = mctx()
        c.font = font
        const t = c.measureText('Hg')
        m = { asc: t.fontBoundingBoxAscent, desc: t.fontBoundingBoxDescent }
        vmetrics.set(font, m)
    }
    return m
}

/** Forget measured widths — fonts finished loading or the theme swapped them. */
export function resetMeasures(): void {
    widths.clear()
    vmetrics.clear()
}

/**
 * Text baselines land on the device-pixel grid the way the browser's own
 * text does (it snaps them; a canvas would draw at the exact fraction).
 */
function snapBase(ctx: CanvasRenderingContext2D, y: number): number {
    // Floor matched Chrome's own placement best (pixel diff against the HTML
    // twin: none/round/ceil 3.27, floor 2.83).
    const d = ctx.getTransform().d || 1
    return Math.floor(y * d) / d
}

// ---- inline layout ----------------------------------------------------------

interface Span {
    text: string
    font: string
    color: string
    code?: boolean
    link?: boolean
    strike?: boolean
    /** Identity for joining a code span's pieces into one background box. */
    id: number
}

interface Frag {
    x: number
    w: number
    text: string
    span: Span
}

interface Line {
    top: number
    frags: Frag[]
}

interface Flow {
    lines: Line[]
    height: number
}

/**
 * Break spans into lines `maxW` wide, CSS `white-space: normal` style:
 * breaks at spaces, collapsed runs of spaces, and after a hyphen inside a
 * word as the browser does ("write-" / "verdict");
 * nothing else is broken mid-word (an overlong word overflows and the box
 * clips it, as in the card). `pre` keeps spaces and breaks only at newlines.
 */
/** A word's pieces with a break opportunity after each hyphen that has text
 *  after it — Chromium's rule, probed: "-flag", "0.10-3" and "a--b" all break
 *  after their hyphens. The pieces flow with no space between them. */
export function hyphenPieces(word: string): string[] {
    return word.split(/(?<=-)(?=.)/)
}

function flow(spans: Span[], x0: number, maxW: number, lh: number, pre = false, maxLines = Infinity, codePad = 0): Flow {
    const lines: Line[] = [{ top: 0, frags: [] }]
    let x = x0
    let pendingSpace = false
    let line = lines[0]!
    const newline = (): boolean => {
        if (lines.length >= maxLines) return false
        line = { top: lines.length * lh, frags: [] }
        lines.push(line)
        x = x0
        pendingSpace = false
        return true
    }
    outer: for (const span of spans) {
        const padX = span.code ? codePad : 0
        x += padX
        const parts = pre ? span.text.split(/(\n)/) : span.text.split(/(\n|[^\S\n]+)/).flatMap(hyphenPieces)
        for (const part of parts) {
            if (part === '') continue
            if (part === '\n') {
                if (!newline()) break outer
                continue
            }
            if (!pre && /^\s+$/.test(part)) {
                if (line.frags.length > 0) pendingSpace = true
                continue
            }
            const space = pendingSpace ? measure(span.font, ' ') : 0
            const w = measure(span.font, part)
            if (!pre && line.frags.length > 0 && x + space + w > x0 + maxW) {
                if (!newline()) break outer
            } else {
                x += space
            }
            pendingSpace = false
            line.frags.push({ x, w, text: part, span })
            x += w
        }
        x += padX
    }
    return { lines, height: lines.length * lh }
}

// ---- markdown ---------------------------------------------------------------

const md = new MarkdownIt({ html: false, linkify: true, breaks: true })

interface Block {
    spans: Span[]
    indent: number
    marginBottom: number
    pre?: boolean
    rule?: boolean
}

let spanSeq = 0

function inlineSpans(tokens: Token[] | null, m: CardMetrics, codeBlock = false): Span[] {
    const out: Span[] = []
    let bold = 0
    let italic = 0
    let link = 0
    let strike = 0
    const b = m.body
    const push = (text: string, code = false): void => {
        const font = code ? b.codeFont : bold ? b.strongFont : italic ? b.emFont : b.text.font
        const color = link ? b.linkColor : bold ? b.strongColor : b.text.color
        out.push({ text, font, color, code, link: link > 0, strike: strike > 0, id: ++spanSeq })
    }
    if (codeBlock) return out
    for (const t of tokens ?? []) {
        switch (t.type) {
            case 'text':
                push(t.content)
                break
            case 'code_inline':
                push(t.content, true)
                break
            case 'softbreak':
            case 'hardbreak':
                push('\n')
                break
            case 'strong_open':
                bold++
                break
            case 'strong_close':
                bold--
                break
            case 'em_open':
                italic++
                break
            case 'em_close':
                italic--
                break
            case 's_open':
                strike++
                break
            case 's_close':
                strike--
                break
            case 'link_open':
                link++
                break
            case 'link_close':
                link--
                break
            case 'image':
                push(t.content)
                break
        }
    }
    return out
}

const blockCache = new Map<string, { epoch: number; blocks: Block[] }>()

/** Markdown → blocks, with the card's CSS: Tailwind's preflight unstyles
 * headings and list markers, MarkdownView adds paragraph and list margins. */
function blocksOf(body: string, m: CardMetrics): Block[] {
    const hit = blockCache.get(body)
    if (hit && hit.epoch === m.epoch) return hit.blocks
    const tokens = md.parse(body, {})
    const blocks: Block[] = []
    let depth = 0
    let row: Span[] | null = null
    for (const t of tokens) {
        switch (t.type) {
            case 'paragraph_open':
            case 'heading_open':
                break
            case 'inline':
                if (row) {
                    if (row.length) row.push({ ...row[0]!, text: '  ', code: false, id: ++spanSeq })
                    row.push(...inlineSpans(t.children, m))
                } else {
                    blocks.push({
                        spans: inlineSpans(t.children, m),
                        indent: depth * m.body.listIndent,
                        marginBottom: 0,
                    })
                }
                break
            case 'paragraph_close':
                if (!t.hidden && blocks.length) blocks[blocks.length - 1]!.marginBottom = m.body.pMargin
                break
            case 'bullet_list_open':
            case 'ordered_list_open':
                depth++
                break
            case 'bullet_list_close':
            case 'ordered_list_close': {
                depth--
                const last = blocks[blocks.length - 1]
                if (last) last.marginBottom = Math.max(last.marginBottom, m.body.pMargin)
                break
            }
            case 'fence':
            case 'code_block':
                blocks.push({
                    spans: [
                        {
                            text: t.content.replace(/\n$/, ''),
                            font: m.body.codeFont,
                            color: m.body.text.color,
                            code: true,
                            id: ++spanSeq,
                        },
                    ],
                    indent: depth * m.body.listIndent,
                    marginBottom: 0,
                    pre: true,
                })
                break
            case 'hr':
                blocks.push({ spans: [], indent: 0, marginBottom: 0, rule: true })
                break
            case 'tr_open':
                row = []
                break
            case 'tr_close':
                if (row) blocks.push({ spans: row, indent: depth * m.body.listIndent, marginBottom: 0 })
                row = null
                break
        }
    }
    if (blockCache.size > 4000) blockCache.clear()
    blockCache.set(body, { epoch: m.epoch, blocks })
    return blocks
}

interface PlacedBlock {
    top: number
    flow: Flow
    block: Block
}

/** Lay out body blocks until they pass `limit` px — the clip hides the rest. */
function layoutBody(body: string, m: CardMetrics, width: number, limit: number): { placed: PlacedBlock[]; height: number } {
    const placed: PlacedBlock[] = []
    let y = 0
    const lh = m.body.text.lh
    const blocks = blocksOf(body, m)
    for (let i = 0; i < blocks.length; i++) {
        const block = blocks[i]!
        if (block.rule) {
            placed.push({ top: y, flow: { lines: [], height: 1 }, block })
            y += 1
        } else {
            const maxLines = Math.max(1, Math.ceil((limit - y) / lh) + 1)
            const f = flow(block.spans, block.indent, width - block.indent, lh, block.pre, maxLines, m.body.codePadX)
            placed.push({ top: y, flow: f, block })
            y += f.height
        }
        if (i < blocks.length - 1) y += block.marginBottom
        if (y > limit) break
    }
    return { placed, height: y }
}

// ---- chips ------------------------------------------------------------------

interface Chip {
    text: string
    style: CardMetrics['typePill']
    fg: string
    bg: string
    border?: string
    w: number
}

function chip(text: string, style: CardMetrics['typePill'], fg: string, bg: string, border?: string, maxW = Infinity): Chip {
    const tw = measure(style.text.font, text)
    return { text, style, fg, bg, border, w: Math.min(tw + 2 * (style.padX + style.border), maxW) }
}

function drawChip(ctx: CanvasRenderingContext2D, c: Chip, x: number, y: number): void {
    const s = c.style
    const h = s.height
    ctx.beginPath()
    ctx.roundRect(x, y, c.w, h, s.radius)
    ctx.fillStyle = c.bg
    ctx.fill()
    if (c.border && s.border > 0) {
        ctx.beginPath()
        ctx.roundRect(x + s.border / 2, y + s.border / 2, c.w - s.border, h - s.border, Math.max(0, s.radius - s.border / 2))
        ctx.lineWidth = s.border
        ctx.strokeStyle = c.border
        ctx.stroke()
    }
    const inner = c.w - 2 * (s.padX + s.border)
    ctx.font = s.text.font
    ctx.fillStyle = c.fg
    const text = ellipsize(c.text, s.text.font, inner)
    ctx.fillText(text, x + s.border + s.padX, snapBase(ctx, y + s.border + s.padY + s.text.base))
}

function ellipsize(text: string, font: string, maxW: number): string {
    if (measure(font, text) <= maxW + 0.5) return text
    let lo = 0
    let hi = text.length
    while (lo < hi) {
        const mid = (lo + hi + 1) >> 1
        if (measure(font, text.slice(0, mid) + '…') <= maxW) lo = mid
        else hi = mid - 1
    }
    return text.slice(0, lo).trimEnd() + '…'
}

/** Chips in wrapping rows; returns each chip's spot and the full rows' height. */
function chipRows(chips: Chip[], width: number, gap: number): { spots: { c: Chip; x: number; y: number }[]; height: number } {
    const spots: { c: Chip; x: number; y: number }[] = []
    let x = 0
    let y = 0
    let rowH = 0
    for (const c of chips) {
        if (x > 0 && x + c.w > width) {
            x = 0
            y += rowH + gap
            rowH = 0
        }
        spots.push({ c, x, y })
        x += c.w + gap
        rowH = Math.max(rowH, c.style.height)
    }
    return { spots, height: chips.length ? y + rowH : 0 }
}

// ---- the card ---------------------------------------------------------------

function trustChip(n: GraphNode, m: CardMetrics, t: Tokens): Chip {
    const c = t.color
    const tinted = (base: RGBA, bg = 0.16, border = 0.4): [string, string, string] => [css(base), css(fade(base, bg)), css(fade(base, border))]
    let label: string
    let colors: [string, string, string]
    if (n.trust_override != null) {
        label = '📌 pinned'
        colors = tinted(c['interactive-primary'], 0.14)
    } else if (n.stale) {
        label = 'stale'
        colors = tinted(c['node-problem'])
    } else if (n.source === 'user') {
        label = 'user'
        colors = tinted(c['trust-trusted'])
    } else if (n.approved_at != null || n.trust >= 0.7) {
        label = 'trusted'
        colors = tinted(c['trust-trusted'])
    } else {
        label = 'provisional'
        colors = tinted(c['trust-provisional'])
    }
    return chip(label, m.trust, ...colors)
}

export function layoutCard(n: GraphNode, m: CardMetrics, t: Tokens, accent: RGBA): CardLayout {
    const W = m.width
    const cw = W - 2 * (m.padX + m.border)
    const left = m.border + m.padX
    const c = t.color

    // Header: type, durability, status … trust badge pushed right.
    const head: Chip[] = [chip(n.type, m.typePill, css(accent), css(fade(accent, 0.16)), css(fade(accent, 0.45)))]
    head.push(chip(n.durability, m.meta, css(c['text-tertiary']), css(c['surface-muted'])))
    if (n.status) head.push(chip(n.status, m.meta, css(c['text-tertiary']), css(c['surface-muted'])))
    const trust = trustChip(n, m, t)

    // One extra line tells whether the clamp cut anything (ellipsis on line 5).
    const titleFull = flow([{ text: n.title, font: m.title.font, color: m.title.color, id: 0 }], 0, cw, m.title.lh, false, m.clamp + 1)
    const titleClamped = titleFull.lines.length > m.clamp
    const titleLines = titleFull.lines.slice(0, m.clamp)
    const title = { lines: titleLines, height: titleLines.length * m.title.lh }

    const body = n.body ?? ''
    const bodyLayout = body ? layoutBody(body, m, cw, m.body.maxH) : null
    let bodyH = bodyLayout ? Math.min(bodyLayout.height, m.body.maxH) : 0

    const refs = n.code_refs.length
        ? chipRows(
              n.code_refs.map((r) => chip(r, m.ref, css(c['text-tertiary']), css(c['surface-sunken']), undefined, cw)),
              cw,
              m.refsGap,
          )
        : null
    const tags = n.tags.length
        ? chipRows(
              n.tags.map((tg) =>
                  chip(`#${tg}`, m.tag, css(c['interactive-primary']), css(fade(c['interactive-primary'], 0.12)), css(fade(c['interactive-primary'], 0.35))),
              ),
              cw,
              m.refsGap,
          )
        : null
    const refsH = refs ? Math.min(refs.height, m.refsMaxH) : 0
    const tagsH = tags ? Math.min(tags.height, m.refsMaxH) : 0

    const frame = 2 * (m.border + m.padY)
    let total = frame + m.headerH + m.gap + title.height
    if (bodyLayout) total += m.gap + bodyH
    if (refs) total += m.gap + refsH
    if (tags) total += m.gap + tagsH
    if (total > m.maxH && bodyLayout) {
        const cut = Math.min(bodyH, total - m.maxH)
        bodyH -= cut
        total -= cut
    }
    const h = Math.min(Math.max(total, m.minH), m.maxH)
    const archived = n.valid_until != null

    function paint(ctx: CanvasRenderingContext2D): void {
        ctx.textBaseline = 'alphabetic'
        let y = m.border + m.padY

        // header
        let x = left
        for (const ch of head) {
            drawChip(ctx, ch, x, y + (m.headerH - ch.style.height) / 2)
            x += ch.w + m.headerGap
        }
        drawChip(ctx, trust, left + cw - trust.w, y + (m.headerH - trust.style.height) / 2)
        y += m.headerH + m.gap

        // title (clamped, ellipsis on the last line like -webkit-line-clamp)
        ctx.save()
        ctx.beginPath()
        ctx.rect(left, y, cw, title.height)
        ctx.clip()
        ctx.font = m.title.font
        ctx.fillStyle = m.title.color
        title.lines.forEach((line, i) => {
            const last = i === title.lines.length - 1 && titleClamped
            const by = snapBase(ctx, y + line.top + m.title.base)
            if (last) {
                const text = line.frags.map((f) => f.text).join(' ')
                ctx.fillText(ellipsize(text + '…', m.title.font, cw), left, by)
            } else {
                for (const f of line.frags) ctx.fillText(f.text, left + f.x, by)
            }
        })
        ctx.restore()
        y += title.height

        if (bodyLayout) {
            y += m.gap
            ctx.save()
            ctx.beginPath()
            ctx.rect(left, y, cw, bodyH)
            ctx.clip()
            paintBody(ctx, bodyLayout.placed, left, y, cw)
            ctx.restore()
            y += bodyH
        }

        for (const rows of [refs, tags]) {
            if (!rows) continue
            const rh = Math.min(rows.height, m.refsMaxH)
            y += m.gap
            ctx.save()
            ctx.beginPath()
            ctx.rect(left - 1, y, cw + 2, rh)
            ctx.clip()
            for (const s of rows.spots) if (s.y < rh) drawChip(ctx, s.c, left + s.x, y + s.y)
            ctx.restore()
            y += rh
        }

        if (archived) {
            const a = m.archived
            ctx.font = a.text.font
            ctx.fillStyle = a.text.color
            const tw = measure(a.text.font, 'archived')
            ctx.fillText('archived', W - m.border - a.right - tw, snapBase(ctx, m.border + a.top + a.text.base))
        }
    }

    function paintBody(ctx: CanvasRenderingContext2D, placed: PlacedBlock[], x0: number, y0: number, width: number): void {
        const codeBg = css(c['surface-sunken'])
        let curFont = ''
        let curColor = ''
        for (const pb of placed) {
            const top = y0 + pb.top
            if (pb.block.rule) {
                ctx.fillStyle = curColor = m.body.text.color
                ctx.fillRect(x0, top, width, 1)
                continue
            }
            for (const line of pb.flow.lines) {
                const lt = top + line.top
                const by = snapBase(ctx, lt + m.body.text.base)
                // code backgrounds first, one box per span per line
                let i = 0
                while (i < line.frags.length) {
                    const f = line.frags[i]!
                    if (!f.span.code) {
                        i++
                        continue
                    }
                    let j = i
                    while (j + 1 < line.frags.length && line.frags[j + 1]!.span.id === f.span.id) j++
                    const end = line.frags[j]!
                    const bx = x0 + f.x - m.body.codePadX
                    const bw = end.x + end.w - f.x + 2 * m.body.codePadX
                    const { asc, desc } = vmetric(f.span.font)
                    ctx.beginPath()
                    ctx.roundRect(bx, by - asc - 1, bw, asc + desc + 2, m.body.codeRadius)
                    ctx.fillStyle = curColor = codeBg
                    ctx.fill()
                    i = j + 1
                }
                for (const f of line.frags) {
                    // Font strings parse slowly in Chrome: set them only on change.
                    if (f.span.font !== curFont) ctx.font = curFont = f.span.font
                    if (f.span.color !== curColor) ctx.fillStyle = curColor = f.span.color
                    ctx.fillText(f.text, x0 + f.x, by)
                    if (f.span.link || f.span.strike) {
                        const size = m.body.text.size
                        const ly = f.span.link ? by + Math.max(1, size * 0.12) : by - size * 0.3
                        ctx.fillRect(x0 + f.x, ly, f.w, Math.max(1, size / 14))
                    }
                }
            }
        }
    }

    return { h, parts: { header: m.headerH, title: title.height, body: bodyH, refs: refsH, tags: tagsH }, paint }
}
