<script setup lang="ts">
import { computed, onMounted, ref, useTemplateRef } from 'vue'
import { onClickOutside } from '@vueuse/core'
import { storeToRefs } from 'pinia'
import { api } from '@/services/api'
import { useProjectsStore } from '@/stores/projects'
import type { FsListing, ProjectInfo } from '@/types/graph'

/**
 * The project switcher (PLAN §7C) — deliberately simple: pick a graph from
 * the machine registry (current project, home graph, siblings), or add one
 * with the daemon-backed folder picker (a browser can't reveal absolute
 * paths; the daemon lists directories instead — GET /fs/dirs). Hidden
 * entirely against a pre-0.6 daemon.
 */
const store = useProjectsStore()
const { projects, active } = storeToRefs(store)

const open = ref(false)
const root = useTemplateRef<HTMLElement>('root')
onClickOutside(root, () => {
    open.value = false
    browsing.value = false
})

const busy = ref(false)
const note = ref('')

onMounted(() => store.loadProjects())

const label = computed(() => {
    const a = active.value
    if (!a) return null
    return a.home ? 'home graph' : a.name
})

function subtitle(p: ProjectInfo): string {
    if (p.home) return 'user-level canon, shared across projects'
    return p.root ?? p.db
}

async function pick(p: ProjectInfo): Promise<void> {
    open.value = false
    note.value = ''
    try {
        await store.switchTo(p)
    } catch (e) {
        note.value = e instanceof Error ? e.message : String(e)
    }
}

// --- the folder picker -----------------------------------------------------

const browsing = ref(false)
const listing = ref<FsListing | null>(null)
const pathDraft = ref('')

async function navigate(path?: string): Promise<void> {
    note.value = ''
    try {
        listing.value = await api.fsDirs(path)
        pathDraft.value = listing.value.path
    } catch (e) {
        note.value = e instanceof Error ? e.message : String(e)
    }
}

async function startBrowsing(): Promise<void> {
    browsing.value = true
    if (!listing.value) await navigate()
}

async function addCurrent(): Promise<void> {
    const path = listing.value?.path
    if (!path) return
    busy.value = true
    note.value = ''
    try {
        await store.addByPath(path)
        browsing.value = false
    } catch (e) {
        note.value = e instanceof Error ? e.message : String(e)
    } finally {
        busy.value = false
    }
}
</script>

<template>
<div v-if="projects.length" ref="root" class="switcher-root">
    <button
        class="toggle"
        type="button"
        :class="{ active: open }"
        :title="'Switch project graph'"
        @click="open = !open"
    >
        <span class="name">{{ label ?? 'projects' }}</span>
        <svg class="chevron" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M6 9l6 6 6-6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
    </button>

    <Transition name="pop">
        <div v-if="open" class="popover glass-panel">
            <h3 class="group-title">Project graphs</h3>
            <button
                v-for="p in projects"
                :key="p.id"
                class="project-row"
                type="button"
                :class="{ on: (active?.id ?? '') === p.id }"
                :title="subtitle(p)"
                @click="pick(p)"
            >
                <span class="row-name">
                    {{ p.home ? 'home graph' : p.name }}
                    <span v-if="p.current" class="badge">this repo</span>
                    <span v-else-if="p.home" class="badge home">shared</span>
                </span>
                <span class="row-path">{{ subtitle(p) }}</span>
            </button>

            <button v-if="!browsing" class="browse-open" type="button" @click="startBrowsing">
                + Add project…
            </button>

            <div v-else class="browser">
                <div class="browser-bar">
                    <button
                        class="nav-btn"
                        type="button"
                        title="Up one folder"
                        :disabled="!listing?.parent"
                        @click="listing?.parent && navigate(listing.parent)"
                    >
                        ↑
                    </button>
                    <button
                        class="nav-btn"
                        type="button"
                        title="Home directory"
                        :disabled="!listing?.home"
                        @click="listing?.home && navigate(listing.home)"
                    >
                        ~
                    </button>
                    <input
                        v-model="pathDraft"
                        class="path-input"
                        type="text"
                        spellcheck="false"
                        title="Current folder — edit and press Enter to jump"
                        @keydown.enter.prevent="navigate(pathDraft.trim())"
                    />
                </div>
                <div class="dir-list">
                    <button
                        v-for="d in listing?.dirs ?? []"
                        :key="d.path"
                        class="dir-row"
                        type="button"
                        :title="d.path"
                        @click="navigate(d.path)"
                    >
                        <svg class="folder" viewBox="0 0 24 24" aria-hidden="true">
                            <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
                        </svg>
                        <span class="dir-name">{{ d.name }}</span>
                        <span v-if="d.engram" class="badge home">engram</span>
                        <span v-else-if="d.git" class="badge">git</span>
                    </button>
                    <p v-if="listing && !listing.dirs.length" class="note">no subfolders here</p>
                </div>
                <div class="browser-actions">
                    <button
                        class="add-btn"
                        type="button"
                        :disabled="busy || !listing"
                        :title="listing ? `Register ${listing.path}` : ''"
                        @click="addCurrent"
                    >
                        {{ busy ? '…' : 'Add this folder' }}
                    </button>
                    <button class="cancel-btn" type="button" @click="browsing = false">Cancel</button>
                </div>
            </div>
            <p v-if="note" class="note">{{ note }}</p>
        </div>
    </Transition>
</div>
</template>

<style scoped>
.switcher-root {
    position: relative;
}

.toggle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    max-width: 18rem;
    padding: 0.5rem 1rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.toggle:hover,
.toggle.active {
    color: var(--text-primary);
    background-color: var(--node-hover-surface);
}

.name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.chevron {
    flex: none;
    width: 1.2rem;
    height: 1.2rem;
}

.popover {
    position: absolute;
    top: calc(100% + 0.8rem);
    left: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    width: 30rem;
    max-height: 60vh;
    overflow-y: auto;
    padding: 1.2rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
}

.pop-enter-active,
.pop-leave-active {
    transition:
        transform var(--duration-fast) var(--ease-default),
        opacity var(--duration-fast) var(--ease-default);
}

.pop-enter-from,
.pop-leave-to {
    transform: translateY(-0.4rem);
    opacity: 0;
}

.group-title {
    margin-bottom: 0.4rem;
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.project-row {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.1rem;
    padding: 0.6rem 0.8rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    cursor: pointer;
}

.project-row:hover {
    background-color: var(--interactive-ghost-hover);
}

.project-row.on {
    background-color: color-mix(in srgb, var(--interactive-primary) 12%, transparent);
}

.row-name {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-weight: 600;
}

.badge {
    padding: 0.1rem 0.6rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-tertiary);
    background-color: var(--surface-muted);
}

.badge.home {
    color: var(--interactive-primary);
    background-color: color-mix(in srgb, var(--interactive-primary) 14%, transparent);
}

.row-path {
    color: var(--text-tertiary);
    font-size: var(--text-caption);
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.browse-open {
    margin-top: 0.6rem;
    padding: 0.6rem 1rem;
    border-radius: var(--radius-md);
    border: 1px dashed var(--border-default);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
    text-align: left;
}

.browse-open:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.browser {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    margin-top: 0.6rem;
    padding-top: 0.8rem;
    border-top: 1px solid var(--border-default);
}

.browser-bar {
    display: flex;
    gap: 0.4rem;
}

.nav-btn {
    flex: none;
    width: 2.8rem;
    padding: 0.5rem 0;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-elevated);
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.nav-btn:disabled {
    opacity: 0.4;
    cursor: default;
}

.nav-btn:hover:not(:disabled) {
    color: var(--text-primary);
    background-color: var(--node-hover-surface);
}

.path-input {
    flex: 1;
    min-width: 0;
    padding: 0.5rem 0.8rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background-color: var(--surface-sunken);
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
}

.dir-list {
    display: flex;
    flex-direction: column;
    max-height: 22rem;
    overflow-y: auto;
}

.dir-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-width: 0;
    padding: 0.4rem 0.6rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    cursor: pointer;
}

.dir-row:hover {
    background-color: var(--interactive-ghost-hover);
}

.folder {
    flex: none;
    width: 1.4rem;
    height: 1.4rem;
    color: var(--text-tertiary);
}

.dir-name {
    min-width: 0;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.dir-row .badge {
    margin-left: auto;
}

.browser-actions {
    display: flex;
    gap: 0.6rem;
}

.add-btn {
    flex: none;
    padding: 0.6rem 1rem;
    border-radius: var(--radius-md);
    border: 1px solid color-mix(in srgb, var(--interactive-primary) 55%, transparent);
    background-color: color-mix(in srgb, var(--interactive-primary) 14%, transparent);
    color: var(--interactive-primary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.add-btn:disabled {
    opacity: 0.5;
    cursor: default;
}

.add-btn:hover:not(:disabled) {
    background-color: color-mix(in srgb, var(--interactive-primary) 24%, transparent);
}

.cancel-btn {
    flex: none;
    padding: 0.6rem 1rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-default);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.cancel-btn:hover {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.note {
    font-size: var(--text-caption);
    color: var(--node-problem);
}
</style>
