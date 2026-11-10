/**
 * `?embed` — the demo running inside the documentation site's landing page,
 * driven by its scroll (`tour.ts`). It shares an origin with the full demo at
 * /demo/ and with the docs, so its storage is the visitor's real storage: a
 * tour that switches themes, screens and tasks would otherwise leave the
 * docs in the tour's last theme and the full demo on the tour's last screen,
 * with the tour's task moves in its graph.
 *
 * So an embedded pane gets in-memory `localStorage` and `sessionStorage`:
 * every load starts from the shipped graph, and nothing it does outlives the
 * frame. This module must evaluate before anything reads storage — engine.ts
 * restores its world at import time, so it imports this first.
 */

export const embedded = new URLSearchParams(location.search).has('embed')

class MemoryStorage implements Storage {
    #items = new Map<string, string>()

    get length(): number {
        return this.#items.size
    }

    clear(): void {
        this.#items.clear()
    }

    getItem(key: string): string | null {
        return this.#items.get(key) ?? null
    }

    key(index: number): string | null {
        return [...this.#items.keys()][index] ?? null
    }

    removeItem(key: string): void {
        this.#items.delete(key)
    }

    setItem(key: string, value: string): void {
        this.#items.set(key, String(value))
    }
}

const SKINS = ['engram-purple', 'vscode-light', 'vscode-dark']

if (embedded) {
    const local = new MemoryStorage()
    // Open on the graph, in the page's own skin (`?theme=`).
    const theme = new URLSearchParams(location.search).get('theme')
    local.setItem('engram.theme', theme && SKINS.includes(theme) ? theme : 'engram-purple')
    local.setItem('engram.view', 'graph')
    local.setItem('engram.layout', 'skyline')
    // The history-recording notice is for a person at their own pane; in a
    // tour it only covers the corner of every scene.
    for (const project of ['lantern', 'home', 'local']) local.setItem(`engram-history-ack:${project}`, '1')
    for (const [name, store] of [
        ['localStorage', local],
        ['sessionStorage', new MemoryStorage()],
    ] as const) {
        Object.defineProperty(window, name, { configurable: true, get: () => store })
    }
}
