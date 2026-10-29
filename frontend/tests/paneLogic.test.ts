// The pane's small pure rules: selection focus rings, code-ref parsing, audit
// labels, and where the GPU card painter may break a word. Run with
// `bun test` — no browser needed.
import { describe, expect, test } from 'bun:test'
import { FOCUS_RINGS, edgeFocus, focusWidth, selectionHops } from '../src/components/graph/focus'
import { hyphenPieces } from '../src/components/graph/cardPainter'
import { parseCodeRefs } from '../src/utils/codeRefs'
import { actionLabel } from '../src/utils/auditAction'

const edge = (from_id: string, to_id: string) => ({ from_id, to_id })

describe('selection focus', () => {
    // a — b — c — d — e, plus a lone pair x — y.
    const chain = [edge('a', 'b'), edge('c', 'b'), edge('c', 'd'), edge('d', 'e'), edge('x', 'y')]

    test('hops walk edges both ways and stop before the last ring', () => {
        const hops = selectionHops('a', chain)
        expect(hops.get('a')).toBe(0)
        expect(hops.get('b')).toBe(1)
        expect(hops.get('c')).toBe(2) // reached against the edge's direction
        expect(hops.has('d')).toBe(false) // FOCUS_RINGS - 1 = 2 hops out
        expect(hops.has('x')).toBe(false)
        expect(FOCUS_RINGS).toBe(3)
    })

    test('a node reached two ways keeps its shortest hop', () => {
        const hops = selectionHops('a', [edge('a', 'b'), edge('b', 'c'), edge('a', 'c')])
        expect(hops.get('c')).toBe(1)
    })

    test('an unknown selection is its own ring only', () => {
        expect([...selectionHops('zz', chain)]).toEqual([['zz', 0]])
    })

    test('edges get rings 1..3 by their nearer end, -1 past them, 0 with no selection', () => {
        const hops = selectionHops('a', chain)
        expect(edgeFocus(null, 'a', 'b')).toBe(0)
        expect(edgeFocus(hops, 'a', 'b')).toBe(1)
        expect(edgeFocus(hops, 'c', 'b')).toBe(2)
        expect(edgeFocus(hops, 'c', 'd')).toBe(3)
        expect(edgeFocus(hops, 'd', 'e')).toBe(-1)
        expect(edgeFocus(hops, 'x', 'y')).toBe(-1)
    })

    test('ring widths fall off ×2, ×1.4, ×1.16', () => {
        expect(focusWidth(1)).toBe(2)
        expect(focusWidth(2)).toBeCloseTo(1.4)
        expect(focusWidth(3)).toBeCloseTo(1.16)
    })
})

describe('card painter word breaks', () => {
    // Expectations probed in Chromium (Playwright, a 1px-wide box): the
    // browser breaks after every hyphen that has text after it.
    test('a break opportunity after each hyphen, kept on the piece before it', () => {
        expect(hyphenPieces('write-verdict')).toEqual(['write-', 'verdict'])
        expect(hyphenPieces('opt-in-by-default')).toEqual(['opt-', 'in-', 'by-', 'default'])
        expect(hyphenPieces('-flag')).toEqual(['-', 'flag'])
        expect(hyphenPieces('0.10-3')).toEqual(['0.10-', '3'])
        expect(hyphenPieces('a--b')).toEqual(['a-', '-', 'b'])
        expect(hyphenPieces('(non-blocking)')).toEqual(['(non-', 'blocking)'])
    })

    test('no break after a final hyphen or in a word without one', () => {
        for (const word of ['plain', 'trailing-', '-', 'snake_case']) {
            expect(hyphenPieces(word)).toEqual([word])
        }
    })

    test('the pieces put back together are the word', () => {
        for (const word of ['kebab-case-slug', 'per-graph', 'self-updating']) {
            expect(hyphenPieces(word).join('')).toBe(word)
        }
    })
})

describe('code refs', () => {
    test('one per line, trimmed, blanks dropped, first occurrence kept', () => {
        expect(parseCodeRefs('  src/a.rs \n\nsrc/b.rs\nsrc/a.rs\r\n   \nsrc/c.rs:12')).toEqual([
            'src/a.rs',
            'src/b.rs',
            'src/c.rs:12',
        ])
    })

    test('empty text is no refs', () => {
        expect(parseCodeRefs('')).toEqual([])
        expect(parseCodeRefs('\n \n')).toEqual([])
    })
})

describe('audit action labels', () => {
    test('a compound action splits into verb and noun', () => {
        expect(actionLabel('node_update')).toEqual({ verb: 'update', noun: 'node' })
        expect(actionLabel('custom_field_set')).toEqual({ verb: 'set', noun: 'custom field' })
    })

    test('known nouns read as words', () => {
        expect(actionLabel('mcp_session_bind')).toEqual({ verb: 'bind', noun: 'MCP session' })
        expect(actionLabel('auto_touch')).toEqual({ verb: 'touch', noun: 'retrieval' })
    })

    test('a single word is its own verb; stray underscores are ignored', () => {
        expect(actionLabel('merge')).toEqual({ verb: 'merge', noun: null })
        expect(actionLabel('_delete_')).toEqual({ verb: '_delete_', noun: null })
        expect(actionLabel('edge__create')).toEqual({ verb: 'create', noun: 'edge' })
    })
})

describe('confirmDialog', () => {
    test('the host settles the open question', async () => {
        const { confirmDialog, useConfirmState } = await import('../src/composables/useConfirm')
        const { pending, settle } = useConfirmState()
        const answer = confirmDialog({ title: 'Delete?', message: 'Gone for good.', danger: true })
        expect(pending.value?.title).toBe('Delete?')
        settle(true)
        expect(await answer).toBe(true)
        expect(pending.value).toBeNull()
    })

    test('a new question answers the pending one with false', async () => {
        const { confirmDialog, useConfirmState } = await import('../src/composables/useConfirm')
        const { pending, settle } = useConfirmState()
        const first = confirmDialog({ title: 'First', message: '' })
        const second = confirmDialog({ title: 'Second', message: '' })
        expect(await first).toBe(false)
        expect(pending.value?.title).toBe('Second')
        settle(false)
        expect(await second).toBe(false)
    })
})
