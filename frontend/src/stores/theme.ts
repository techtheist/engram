import { defineStore } from 'pinia'
import { ref, watch } from 'vue'

export interface ThemeOption {
    id: string
    label: string
}

/** The five skins. `engram-purple` is the brand default (and the only glassy one). */
export const THEMES: ThemeOption[] = [
    { id: 'engram-purple', label: 'Engram Purple' },
    { id: 'jetbrains-dark', label: 'JetBrains Dark' },
    { id: 'jetbrains-light', label: 'JetBrains Light' },
    { id: 'vscode-dark', label: 'VS Code Dark' },
    { id: 'vscode-light', label: 'VS Code Light' },
]

const STORAGE_KEY = 'engram.theme'
const DEFAULT_THEME = 'engram-purple'

function initialTheme(): string {
    const saved = localStorage.getItem(STORAGE_KEY)
    return saved && THEMES.some((t) => t.id === saved) ? saved : DEFAULT_THEME
}

export const useThemeStore = defineStore('theme', () => {
    const current = ref<string>(initialTheme())

    function apply(id: string): void {
        document.documentElement.setAttribute('data-theme', id)
    }

    function set(id: string): void {
        if (!THEMES.some((t) => t.id === id)) return
        current.value = id
    }

    watch(
        current,
        (id) => {
            apply(id)
            localStorage.setItem(STORAGE_KEY, id)
        },
        { immediate: true },
    )

    return { current, themes: THEMES, set }
})
