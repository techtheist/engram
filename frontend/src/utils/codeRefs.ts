/**
 * Code refs are edited as plain text, one per line (no file picker — user
 * decision). Parsing rule: split on newlines, trim each line, drop empty
 * lines, de-duplicate while preserving first-seen order. Used by both the
 * node-detail edit form and the create panel so the two stay identical.
 */
export function parseCodeRefs(text: string): string[] {
    const seen = new Set<string>()
    const out: string[] = []
    for (const raw of text.split('\n')) {
        const line = raw.trim()
        if (!line || seen.has(line)) continue
        seen.add(line)
        out.push(line)
    }
    return out
}
