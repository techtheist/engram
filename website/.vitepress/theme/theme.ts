import { ref } from 'vue'

/** The pane's three static skins, by the pane's own ids. */
export const SKINS = [
  { id: 'engram-purple', label: 'Engram' },
  { id: 'vscode-light', label: 'Light' },
  { id: 'vscode-dark', label: 'Dark' },
] as const

/** The pane's storage key: a skin picked here holds in the demo too. */
const KEY = 'engram.theme'

/**
 * The skin on the page. The head script in config.ts sets it before first
 * paint; the ref starts at the server's default and picks the real one up
 * after hydration (`syncSkin`), so the first client render matches the HTML.
 */
export const skin = ref('engram-purple')

export function syncSkin(): void {
  skin.value = document.documentElement.getAttribute('data-theme') ?? 'engram-purple'
}

export function setSkin(id: string): void {
  skin.value = id
  const root = document.documentElement
  root.setAttribute('data-theme', id)
  // VitePress keys its dark code highlighting on .dark.
  root.classList.toggle('dark', id !== 'vscode-light')
  try {
    localStorage.setItem(KEY, id)
  } catch {
    // storage blocked — the choice lasts this page
  }
}
