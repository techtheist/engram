<script setup lang="ts">
import { ref, useTemplateRef } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { api } from '@/services/api'
import { useGraphStore } from '@/stores/graph'
import { useThemeStore } from '@/stores/theme'
import type { ExportGraph } from '@/types/graph'

const theme = useThemeStore()
const store = useGraphStore()

const open = ref(false)
const root = useTemplateRef<HTMLElement>('root')
onClickOutside(root, () => (open.value = false))

// --- export / import (moved out of the old inline bar) --------------------

const fileInput = ref<HTMLInputElement | null>(null)
const busy = ref(false)
const status = ref<string | null>(null)
const isError = ref(false)
let statusTimer: ReturnType<typeof setTimeout> | undefined

function flash(msg: string, error = false): void {
    status.value = msg
    isError.value = error
    clearTimeout(statusTimer)
    statusTimer = setTimeout(() => (status.value = null), 4000)
}

async function exportGraph(): Promise<void> {
    busy.value = true
    try {
        const graph = await api.exportGraph()
        const blob = new Blob([JSON.stringify(graph, null, 2)], { type: 'application/json' })
        const url = URL.createObjectURL(blob)
        const a = document.createElement('a')
        a.href = url
        a.download = `engram-${stamp()}.json`
        a.click()
        URL.revokeObjectURL(url)
        flash(`Exported ${graph.nodes.length} nodes, ${graph.edges.length} edges`)
    } catch (e) {
        flash(message(e), true)
    } finally {
        busy.value = false
    }
}

function pickFile(): void {
    fileInput.value?.click()
}

async function onFile(event: Event): Promise<void> {
    const input = event.target as HTMLInputElement
    const file = input.files?.[0]
    input.value = ''
    if (!file) return
    busy.value = true
    try {
        const graph = JSON.parse(await file.text()) as ExportGraph
        if (!Array.isArray(graph.nodes) || !Array.isArray(graph.edges)) {
            throw new Error('Not an Engram export (missing nodes/edges)')
        }
        const summary = await api.importGraph(graph)
        await store.load()
        flash(`Imported ${summary.nodes} nodes, ${summary.edges} edges`)
    } catch (e) {
        flash(message(e), true)
    } finally {
        busy.value = false
    }
}

function stamp(): string {
    return new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-')
}

function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e)
}
</script>

<template>
<div ref="root" class="settings">
    <button
        class="gear glass-panel"
        :class="{ active: open }"
        type="button"
        aria-label="Settings"
        :aria-expanded="open"
        @click="open = !open"
    >
        <!-- JetBrains gear icon (Apache 2.0); fill uses currentColor for theming -->
        <svg viewBox="0 0 16 16" fill="none" class="gear-icon" aria-hidden="true">
            <path
                fill-rule="evenodd"
                clip-rule="evenodd"
                fill="currentColor"
                d="M3.22655 4.36961C2.92233 4.76924 2.66735 5.20781 2.47037 5.67626L3.30992 7.0943C3.6406 7.65283 3.6406 8.34718 3.30992 8.9057L2.47037 10.3237C2.66736 10.7922 2.92233 11.2308 3.22656 11.6304L4.87261 11.6124C5.52165 11.6053 6.12297 11.9524 6.44133 12.5181L7.24899 13.953C7.49593 13.984 7.74792 14 8.00408 14C8.2602 14 8.51214 13.984 8.75904 13.9531L9.56671 12.5181C9.88507 11.9524 10.4864 11.6053 11.1354 11.6124L12.7816 11.6304C13.0858 11.2308 13.3408 10.7923 13.5377 10.3239L12.6981 8.9057C12.3674 8.34718 12.3674 7.65283 12.6981 7.0943L13.5377 5.67613C13.3408 5.20773 13.0858 4.76921 12.7816 4.36961L11.1354 4.38764C10.4864 4.39475 9.88507 4.04758 9.56671 3.48194L8.75904 2.04693C8.51214 2.01599 8.2602 2 8.00408 2C7.74792 2 7.49594 2.016 7.24899 2.04695L6.44133 3.48194C6.12297 4.04758 5.52165 4.39475 4.87261 4.38764L3.22655 4.36961ZM10.655 8.00001C10.655 9.46412 9.46811 10.651 8.004 10.651C6.5399 10.651 5.353 9.46412 5.353 8.00001C5.353 6.53591 6.5399 5.34902 8.004 5.34902C9.46811 5.34902 10.655 6.53591 10.655 8.00001ZM4.88356 3.3877C5.16752 3.39081 5.4306 3.23892 5.56988 2.99146L6.43817 1.44875C6.54914 1.25159 6.7401 1.1101 6.96387 1.07676C7.30327 1.0262 7.65062 1 8.00408 1C8.3575 1 8.7048 1.02619 9.04414 1.07674C9.26792 1.11007 9.45889 1.25157 9.56986 1.44873L10.4382 2.99146C10.5774 3.23892 10.8405 3.39081 11.1245 3.3877L12.8938 3.36832C13.1196 3.36585 13.3372 3.46012 13.4781 3.63661C13.9099 4.17766 14.2632 4.78416 14.5207 5.43884C14.6034 5.6491 14.5763 5.88489 14.4612 6.07931L13.5586 7.60376C13.4139 7.84811 13.4139 8.15189 13.5586 8.39625L14.4612 9.92069C14.5763 10.1151 14.6034 10.3509 14.5207 10.5612C14.2632 11.2158 13.9099 11.8223 13.4781 12.3634C13.3372 12.5399 13.1196 12.6342 12.8938 12.6317L11.1245 12.6123C10.8405 12.6092 10.5774 12.7611 10.4382 13.0085L9.56986 14.5513C9.45889 14.7484 9.26792 14.8899 9.04414 14.9233C8.7048 14.9738 8.3575 15 8.00408 15C7.65062 15 7.30327 14.9738 6.96387 14.9232C6.7401 14.8899 6.54914 14.7484 6.43817 14.5512L5.56988 13.0085C5.4306 12.7611 5.16752 12.6092 4.88356 12.6123L3.1144 12.6317C2.8886 12.6342 2.67096 12.5399 2.5301 12.3634C2.09822 11.8223 1.74489 11.2158 1.48738 10.561C1.40469 10.3508 1.43184 10.115 1.54695 9.92057L2.44942 8.39625C2.5941 8.15189 2.5941 7.84811 2.44942 7.60376L1.54695 6.07944C1.43184 5.88502 1.40469 5.64924 1.48738 5.43898C1.74489 4.78425 2.09822 4.1777 2.53009 3.63661C2.67096 3.46012 2.8886 3.36585 3.1144 3.36832L4.88356 3.3877ZM9.655 8.00001C9.655 8.91183 8.91582 9.65101 8.004 9.65101C7.09218 9.65101 6.353 8.91183 6.353 8.00001C6.353 7.08819 7.09218 6.34902 8.004 6.34902C8.91582 6.34902 9.655 7.08819 9.655 8.00001Z"
            />
        </svg>
    </button>

    <Transition name="pop">
        <div v-if="open" class="menu glass-panel" role="menu">
            <div class="section-label">Theme</div>
            <div class="themes">
                <button
                    v-for="t in theme.themes"
                    :key="t.id"
                    class="theme-opt"
                    :class="{ current: t.id === theme.current }"
                    type="button"
                    @click="theme.set(t.id)"
                >
                    <span class="swatch" :data-theme="t.id" aria-hidden="true" />
                    <span class="theme-name">{{ t.label }}</span>
                    <span v-if="t.id === theme.current" class="check" aria-hidden="true">✓</span>
                </button>
            </div>

            <div class="divider" />

            <div class="section-label">Graph</div>
            <button class="row" type="button" :disabled="busy" @click="exportGraph">
                <span class="row-icon">↓</span> Export JSON
            </button>
            <button class="row" type="button" :disabled="busy" @click="pickFile">
                <span class="row-icon">↑</span> Import JSON
            </button>

            <p v-if="status" class="status" :class="{ error: isError }">{{ status }}</p>
        </div>
    </Transition>

    <input ref="fileInput" class="hidden-input" type="file" accept="application/json,.json" @change="onFile" />
</div>
</template>

<style scoped>
.settings {
    position: relative;
}

.gear {
    display: grid;
    place-items: center;
    width: 3.6rem;
    height: 3.6rem;
    border-radius: var(--radius-full);
    color: var(--text-secondary);
    cursor: pointer;
    transition:
        color var(--duration-fast) var(--ease-default),
        transform var(--duration-normal) var(--ease-spring);
}

.gear:hover {
    color: var(--text-primary);
}

.gear.active {
    color: var(--interactive-primary);
    transform: rotate(60deg);
}

.gear-icon {
    width: 2rem;
    height: 2rem;
}

.menu {
    position: absolute;
    top: calc(100% + 0.8rem);
    right: 0;
    z-index: 20;
    width: 24rem;
    max-width: calc(100vw - 3.2rem);
    padding: 0.8rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
}

.section-label {
    padding: 0.4rem 0.8rem;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.themes {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
}

.theme-opt {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    width: 100%;
    padding: 0.6rem 0.8rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    cursor: pointer;
    text-align: left;
}

.theme-opt:hover {
    background-color: var(--interactive-ghost-hover);
}

.theme-opt.current {
    color: var(--interactive-primary);
}

.swatch {
    width: 1.4rem;
    height: 1.4rem;
    flex-shrink: 0;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-default);
    background: linear-gradient(135deg, var(--surface-base) 50%, var(--interactive-primary) 50%);
}

.theme-name {
    flex: 1;
}

.check {
    color: var(--interactive-primary);
    font-size: var(--text-body-sm);
}

.divider {
    height: 1px;
    margin: 0.6rem 0.4rem;
    background-color: var(--border-subtle);
}

.row {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    width: 100%;
    padding: 0.7rem 0.8rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    cursor: pointer;
    text-align: left;
}

.row:disabled {
    opacity: 0.5;
    cursor: default;
}

.row:hover:not(:disabled) {
    background-color: var(--interactive-ghost-hover);
}

.row-icon {
    width: 1.4rem;
    text-align: center;
    color: var(--text-tertiary);
    font-weight: 700;
}

.status {
    margin: 0.4rem 0.4rem 0;
    padding: 0.5rem 0.8rem;
    border-radius: var(--radius-md);
    font-size: var(--text-caption);
    color: var(--text-secondary);
    background-color: var(--surface-muted);
}

.status.error {
    color: var(--node-problem);
}

.hidden-input {
    display: none;
}

.pop-enter-active,
.pop-leave-active {
    transition:
        opacity var(--duration-fast) var(--ease-default),
        transform var(--duration-fast) var(--ease-default);
    transform-origin: top right;
}

.pop-enter-from,
.pop-leave-to {
    opacity: 0;
    transform: scale(0.95) translateY(-0.4rem);
}
</style>
