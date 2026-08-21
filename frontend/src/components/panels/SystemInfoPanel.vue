<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import SidePanel from '@/components/common/SidePanel.vue'
import { api } from '@/services/api'
import { useSystemInfo } from '@/composables/useSystemInfo'
import { useProjectsStore } from '@/stores/projects'
import type { AgentSettings, ModelRoleInfo, ModelSelection, ProjectInfo, SystemInfo } from '@/types/graph'

/**
 * Settings → System info: the daemon-side half of `engram-alpha doctor`,
 * formatted — binary version, store health, embedding model, and which
 * assistants are wired to this repo (GET /system).
 */
const { open, hide } = useSystemInfo()

const info = ref<SystemInfo | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

// The machine registry (PLAN §7C) — ~/.engram/registry.json, via the hub.
const projectsStore = useProjectsStore()
const { projects } = storeToRefs(projectsStore)

watch(open, (isOpen) => {
    if (isOpen) {
        void reload()
        void projectsStore.loadProjects()
    }
})

function projectLabel(p: ProjectInfo): string {
    if (p.home) return 'home graph'
    return p.name
}

async function forget(p: ProjectInfo): Promise<void> {
    await projectsStore.unregister(p.id)
}

async function reload(): Promise<void> {
    loading.value = true
    error.value = null
    try {
        info.value = await api.system()
        selection.value = await api.models().catch(() => null)
        syncPicks()
        // Machine-level settings: a 404 (older or non-core daemon) hides the
        // control instead of breaking the panel.
        agentSettings.value = await api.settings().catch(() => null)
        agentPick.value = agentSettings.value?.default_agent_project ?? ''
    } catch (e) {
        error.value = e instanceof Error ? e.message : String(e)
        info.value = null
    } finally {
        loading.value = false
    }
}

// ---- default agent project (machine-level, GET/POST /settings) -------------

const agentSettings = ref<AgentSettings | null>(null)
/** The dropdown's value: a project id, or '' for the home graph. */
const agentPick = ref('')
const agentSaving = ref(false)
const agentNote = ref('')

async function applyAgentDefault(): Promise<void> {
    agentSaving.value = true
    agentNote.value = ''
    try {
        agentSettings.value = await api.saveSettings(agentPick.value || null)
        agentPick.value = agentSettings.value.default_agent_project ?? ''
        agentNote.value = agentSettings.value.default_agent_project_name
            ? `saved — new agent sessions without a folder bind ${agentSettings.value.default_agent_project_name}`
            : 'saved — new agent sessions without a folder bind the home graph'
    } catch (e) {
        agentNote.value = e instanceof Error ? e.message : String(e)
        agentPick.value = agentSettings.value?.default_agent_project ?? ''
    } finally {
        agentSaving.value = false
    }
}

// ---- model selection (PLAN §7A): pick per role, custom by URL --------------

const selection = ref<ModelSelection | null>(null)
/** Per-role UI state: the picked preset name, or 'custom'. */
const picks = ref<Record<string, string>>({})
const customUrl = ref<Record<string, string>>({})
const customName = ref<Record<string, string>>({})
const customDim = ref<Record<string, string>>({})
const customMean = ref<Record<string, boolean>>({})
const applying = ref<string | null>(null)
const applyNote = ref<Record<string, string>>({})

const roles = computed<ModelRoleInfo[]>(() => selection.value?.roles ?? [])

function syncPicks(): void {
    for (const r of roles.value) {
        picks.value[r.role] = r.presets.some((p) => p.name === r.active) ? r.active : 'custom'
        if (r.custom) {
            customName.value[r.role] = r.custom.name
            customUrl.value[r.role] = r.custom.base_url
            customDim.value[r.role] = r.custom.dim ? String(r.custom.dim) : ''
            customMean.value[r.role] = r.custom.pooling === 'mean'
        }
    }
}

function dirty(r: ModelRoleInfo): boolean {
    const pick = picks.value[r.role]
    if (pick === 'custom') return true
    return pick !== r.active
}

async function apply(r: ModelRoleInfo): Promise<void> {
    const pick = picks.value[r.role]
    const body: { role: string; preset?: string; custom?: object } = { role: r.role }
    if (pick === 'custom') {
        const name = customName.value[r.role]?.trim()
        const base_url = customUrl.value[r.role]?.trim()
        if (!name || !base_url) {
            applyNote.value[r.role] = 'a custom model needs a name and a base URL'
            return
        }
        body.custom = {
            name,
            base_url,
            ...(r.role === 'embedding' && {
                dim: Number(customDim.value[r.role]) || undefined,
                pooling: customMean.value[r.role] ? 'mean' : 'cls',
            }),
        }
    } else {
        body.preset = pick
    }
    applying.value = r.role
    applyNote.value[r.role] = ''
    try {
        const result = await api.applyModel(body)
        applyNote.value[r.role] =
            result.reembedded_nodes > 0
                ? `switched to ${result.applied} — re-embedded ${result.reembedded_nodes} nodes`
                : `switched to ${result.applied}`
        await reload()
    } catch (e) {
        applyNote.value[r.role] = e instanceof Error ? e.message : String(e)
    } finally {
        applying.value = null
    }
}

type Status = 'ok' | 'warn' | 'bad'

function fmtBytes(n: number | null): string {
    if (n == null) return '—'
    if (n < 1024) return `${n} B`
    const units = ['KB', 'MB', 'GB']
    let v = n
    let u = -1
    do {
        v /= 1024
        u += 1
    } while (v >= 1024 && u < units.length - 1)
    return `${v.toFixed(1)} ${units[u]}`
}

function fmtUptime(secs: number): string {
    if (secs < 60) return `${secs}s`
    const m = Math.floor(secs / 60)
    if (m < 60) return `${m}m ${secs % 60}s`
    const h = Math.floor(m / 60)
    if (h < 24) return `${h}h ${m % 60}m`
    return `${Math.floor(h / 24)}d ${h % 24}h`
}

/** Humane relative time for the process census — "3m ago", never raw unix. */
function relTime(ts: number): string {
    const secs = Math.max(0, Math.floor(Date.now() / 1000 - ts))
    if (secs < 60) return 'just now'
    const m = Math.floor(secs / 60)
    if (m < 60) return `${m}m ago`
    const h = Math.floor(m / 60)
    if (h < 24) return `${h}h ago`
    return `${Math.floor(h / 24)}d ago`
}

/** Uptime since a unix start instant, in the panel's duration style. */
function upSince(startedAt: number): string {
    return fmtUptime(Math.max(0, Math.floor(Date.now() / 1000 - startedAt)))
}

/** The folder is a client's primary identity — its basename is the headline. */
function basename(path: string): string {
    const parts = path.replace(/\/+$/, '').split('/')
    return parts[parts.length - 1] || path
}

/** The cortex residency badge (0.8.8 idle ONNX unload). */
function modelStateBadge(s: { state: string }): { status: Status; text: string } {
    if (s.state === 'loaded') return { status: 'ok', text: 'active' }
    if (s.state === 'unloaded_idle') return { status: 'warn', text: 'idle (models unloaded)' }
    return { status: 'warn', text: s.state }
}

function embeddingStatus(s: SystemInfo): { status: Status; text: string } {
    const missing = s.store.nodes - s.store.embedded
    if (missing > 0) return { status: 'warn', text: `${missing} node(s) not embedded` }
    return { status: 'ok', text: `all ${s.store.nodes} nodes embedded` }
}

function wiringStatus(w: { wired: boolean; prerename: boolean }): { status: Status; text: string } {
    if (!w.wired) return { status: 'warn', text: 'detected, not wired' }
    if (w.prerename) return { status: 'warn', text: 'wired to the pre-rename binary — re-run setup' }
    return { status: 'ok', text: 'wired' }
}
</script>

<template>
<SidePanel
    :open="open"
    side="left"
    panel-id="system"
    :default-rem="42"
    :min-rem="30"
    :dismiss="hide"
    title="System info"
    style="--panel-gap: 1.2rem"
>
    <p v-if="error" class="state error">{{ error }}</p>
    <p v-else-if="loading && !info" class="state">Loading…</p>

    <template v-if="info">
        <header class="hero">
            <span class="product">Engram Alpha</span>
            <span class="version">v{{ info.version }}</span>
        </header>

        <section class="block">
            <h3 class="block-title">Daemon</h3>
            <dl class="rows">
                <div><dt>Uptime</dt><dd>{{ fmtUptime(info.daemon.uptime_secs) }}</dd></div>
                <div><dt>Process</dt><dd>pid {{ info.daemon.pid }}</dd></div>
                <div><dt>Repository</dt><dd class="mono">{{ info.daemon.repo_root }}</dd></div>
            </dl>
        </section>

        <!-- The process census (0.8.8+): the core plus every connected light
             client. Older daemons don't send `processes` — the section simply
             doesn't exist against a 0.8.7 core. -->
        <section v-if="info.processes" class="block">
            <h3 class="block-title">Processes</h3>

            <div class="proc">
                <div class="proc-head">
                    <span class="proc-name">core</span>
                    <span class="proc-tag">pid {{ info.processes.core.pid }}</span>
                    <span class="proc-tag">v{{ info.processes.core.version }}</span>
                    <span v-if="info.models_state" class="proc-tag badge">
                        <span class="dot" :data-status="modelStateBadge(info.models_state).status" />
                        {{ modelStateBadge(info.models_state).text }}
                    </span>
                </div>
                <span class="mono proc-path" :title="info.processes.core.home">
                    {{ info.processes.core.home }}
                </span>
                <span class="proc-times">up {{ upSince(info.processes.core.started_at) }}</span>
            </div>

            <div v-for="c in info.processes.clients" :key="c.lease_id" class="proc">
                <div class="proc-head">
                    <span class="proc-name">{{ basename(c.root) }}</span>
                    <span class="proc-tag">{{ c.kind }}</span>
                    <!-- Which MCP client bound here (clientInfo.name) —
                         older bridges never sent one, the tag just hides. -->
                    <span v-if="c.client" class="proc-tag" :title="'MCP client: ' + c.client">
                        {{ c.client }}
                    </span>
                    <span class="proc-tag">pid {{ c.pid }}</span>
                </div>
                <span class="mono proc-path" :title="c.root">{{ c.root }}</span>
                <span class="proc-times">
                    connected {{ relTime(c.connected_at) }} · seen {{ relTime(c.last_seen) }}
                </span>
            </div>

            <p v-if="!info.processes.clients.length" class="state">
                No light processes connected.
            </p>
        </section>

        <section class="block">
            <h3 class="block-title">Graph store</h3>
            <dl class="rows">
                <div><dt>Database</dt><dd class="mono">{{ info.store.db ?? '—' }}</dd></div>
                <div>
                    <dt>Backend</dt>
                    <dd>{{ info.store.backend === 'tepindb' ? 'TepinDB (redb single-file)' : 'SQLite' }}</dd>
                </div>
                <div><dt>Size</dt><dd>{{ fmtBytes(info.store.size_bytes) }}</dd></div>
                <div><dt>Contents</dt><dd>{{ info.store.nodes }} nodes · {{ info.store.edges }} edges</dd></div>
                <div>
                    <dt>Integrity</dt>
                    <dd>
                        <span class="dot" :data-status="info.store.integrity_ok ? 'ok' : 'bad'" />
                        {{ info.store.integrity_ok ? 'integrity check passed' : 'integrity check FAILED' }}
                    </dd>
                </div>
                <div>
                    <dt>Journal</dt>
                    <dd>
                        <span class="dot" :data-status="!info.store.journal_mode || info.store.journal_mode === 'wal' ? 'ok' : 'warn'" />
                        {{ info.store.journal_mode || 'fsync per commit (redb)' }}
                    </dd>
                </div>
                <div>
                    <dt>Vectors</dt>
                    <dd>
                        <span class="dot" :data-status="embeddingStatus(info).status" />
                        {{ embeddingStatus(info).text }}
                    </dd>
                </div>
                <div>
                    <dt>Composition</dt>
                    <dd>
                        <span class="dot" :data-status="info.store.embed_composition_current ? 'ok' : 'warn'" />
                        <template v-if="info.store.embed_composition_current">
                            current (title · body · tags · code refs)
                        </template>
                        <template v-else>
                            outdated — restart the daemon with real embeddings to reindex
                        </template>
                    </dd>
                </div>
            </dl>
        </section>

        <section class="block">
            <h3 class="block-title">Local models — the cortex</h3>
            <dl class="rows">
                <div v-for="m in info.models" :key="m.name" :title="m.role">
                    <dt>{{ m.name }}</dt>
                    <dd class="model-cell">
                        <span class="model-status">
                            <span class="dot" :data-status="m.active ? 'ok' : 'warn'" />
                            {{ m.active ? m.role : 'not loaded — downloads on daemon startup, feature degrades gracefully until then' }}
                        </span>
                        <code class="model-path">{{ m.path }}</code>
                    </dd>
                </div>
            </dl>

            <template v-if="selection?.available && roles.length">
                <h4 class="sub-title">Choose models</h4>
                <p v-if="selection.fake_embeddings" class="state">
                    This daemon runs fake embeddings — restart it with real embeddings to switch models.
                </p>
                <div v-for="r in roles" :key="r.role" class="pick">
                    <label class="pick-role" :for="`model-pick-${r.role}`">{{ r.role }}</label>
                    <div class="pick-body">
                        <select :id="`model-pick-${r.role}`" v-model="picks[r.role]" class="pick-select">
                            <option v-for="p in r.presets" :key="p.name" :value="p.name">
                                {{ p.name }}{{ p.name === r.default ? ' (default)' : '' }}
                            </option>
                            <option value="custom">custom — by URL…</option>
                        </select>
                        <template v-if="picks[r.role] === 'custom'">
                            <input
                                v-model="customName[r.role]"
                                class="pick-input"
                                type="text"
                                placeholder="model name (its cache folder)"
                            />
                            <input
                                v-model="customUrl[r.role]"
                                class="pick-input"
                                type="text"
                                placeholder="base URL, e.g. https://huggingface.co/<org>/<repo>/resolve/main"
                            />
                            <div v-if="r.role === 'embedding'" class="pick-inline">
                                <input
                                    v-model="customDim[r.role]"
                                    class="pick-input dim"
                                    type="text"
                                    placeholder="dim (e.g. 384)"
                                />
                                <label class="pick-check">
                                    <input v-model="customMean[r.role]" type="checkbox" />
                                    mean pooling (default: CLS)
                                </label>
                            </div>
                        </template>
                        <p v-if="r.role === 'embedding' && dirty(r)" class="pick-warning">
                            Switching the embedding model re-embeds every node in all open graphs
                            (blocking, one-time) and un-calibrates the tuned similarity thresholds —
                            duplicate/conflict detection quality is unvalidated until re-tuned.
                        </p>
                        <div class="pick-actions">
                            <button
                                class="refresh"
                                type="button"
                                :disabled="applying !== null || !dirty(r) || selection.fake_embeddings"
                                @click="apply(r)"
                            >
                                {{ applying === r.role ? 'Downloading + applying…' : 'Apply' }}
                            </button>
                            <span v-if="applyNote[r.role]" class="pick-note">{{ applyNote[r.role] }}</span>
                        </div>
                    </div>
                </div>
            </template>
        </section>

        <section v-if="projects.length" class="block">
            <h3 class="block-title">Projects on this machine — the registry</h3>
            <div v-for="p in projects" :key="p.id" class="project-line">
                <span class="project-name">
                    {{ projectLabel(p) }}
                    <span v-if="p.current" class="tag">this repo</span>
                    <span v-else-if="p.home" class="tag">shared</span>
                    <span v-else-if="p.open" class="tag">open</span>
                </span>
                <span class="mono project-path">{{ p.root ?? p.db }}</span>
                <button
                    v-if="!p.current && !p.home"
                    class="forget"
                    type="button"
                    title="Remove from the registry (the project's graph itself is untouched)"
                    @click="forget(p)"
                >
                    forget
                </button>
            </div>
        </section>

        <!-- Machine-level default for agent sessions that can't name their
             project (no --db, no usable MCP root, no usable launch cwd —
             e.g. Windsurf's JetBrains plugin spawning bridges from `/`).
             Hidden against an older core: /settings 404s there. -->
        <section v-if="agentSettings" class="block">
            <h3 class="block-title">Default agent project</h3>
            <div class="pick">
                <label class="pick-role" for="agent-default-pick">binds to</label>
                <div class="pick-body">
                    <select
                        id="agent-default-pick"
                        v-model="agentPick"
                        class="pick-select"
                        :disabled="agentSaving"
                        @change="applyAgentDefault"
                    >
                        <option value="">home graph (default)</option>
                        <option v-for="p in projects.filter((p) => !p.home)" :key="p.id" :value="p.id">
                            {{ p.name }}
                        </option>
                    </select>
                    <p class="pick-hint">
                        Agents that can't reveal their folder connect here. Applies to new
                        sessions — already-connected ones keep their project.
                    </p>
                    <p v-if="agentNote" class="pick-note">{{ agentNote }}</p>
                </div>
            </div>
        </section>

        <section class="block">
            <h3 class="block-title">Assistants on this machine</h3>
            <dl class="rows">
                <div v-for="w in info.wiring" :key="w.agent">
                    <dt class="agent">{{ w.agent }}</dt>
                    <dd>
                        <span class="dot" :data-status="wiringStatus(w).status" />
                        {{ wiringStatus(w).text }}
                    </dd>
                </div>
                <p v-if="!info.wiring.length" class="state">No supported assistants detected.</p>
            </dl>
        </section>

        <footer class="foot">
            <button class="refresh" type="button" :disabled="loading" @click="reload">
                {{ loading ? 'Refreshing…' : 'Refresh' }}
            </button>
        </footer>
    </template>
</SidePanel>
</template>

<style scoped>
.state {
    font-size: var(--text-body-sm);
    color: var(--text-secondary);
}

.state.error {
    color: var(--node-problem);
}

.hero {
    display: flex;
    align-items: baseline;
    gap: 0.8rem;
}

.product {
    font-size: var(--text-body);
    font-weight: 700;
    color: var(--text-primary);
}

.version {
    padding: 0.1rem 0.8rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full);
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--interactive-primary);
}

.block {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
}

.block-title {
    font-size: var(--text-caption);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
}

.rows {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
}

.rows > div {
    display: grid;
    grid-template-columns: 10rem 1fr;
    gap: 0.8rem;
    align-items: baseline;
}

.rows dt {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.rows dt.agent {
    font-family: var(--font-mono);
    color: var(--text-secondary);
}

.rows dd {
    min-width: 0;
    font-size: var(--text-body-sm);
    color: var(--text-primary);
    overflow-wrap: anywhere;
}

.rows dd.mono {
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.dot {
    display: inline-block;
    width: 0.8rem;
    height: 0.8rem;
    margin-right: 0.5rem;
    border-radius: var(--radius-full);
    vertical-align: baseline;
}

.dot[data-status='ok'] {
    background-color: var(--trust-trusted);
}

.dot[data-status='warn'] {
    background-color: var(--node-caution);
}

.dot[data-status='bad'] {
    background-color: var(--node-problem);
}

.foot {
    margin-top: auto;
    padding-top: 0.6rem;
}

.refresh {
    padding: 0.4rem 1rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-caption);
    font-weight: 600;
    cursor: pointer;
}

.refresh:disabled {
    opacity: 0.5;
    cursor: default;
}

.refresh:hover:not(:disabled) {
    color: var(--text-primary);
    background-color: var(--interactive-ghost-hover);
}

.model-cell {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    min-width: 0;
}

.model-status {
    display: flex;
    align-items: center;
    gap: 0.6rem;
}

.model-path {
    max-width: 100%;
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    overflow-x: auto;
    white-space: nowrap;
}

.sub-title {
    margin-top: 0.4rem;
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-tertiary);
}

.pick {
    display: grid;
    grid-template-columns: 10rem 1fr;
    gap: 0.8rem;
    align-items: start;
    padding: 0.3rem 0;
}

.pick-role {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    text-transform: capitalize;
    padding-top: 0.4rem;
}

.pick-body {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    min-width: 0;
}

.pick-select,
.pick-input {
    width: 100%;
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md, 0.4rem);
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
}

.pick-inline {
    display: flex;
    align-items: center;
    gap: 0.8rem;
}

.pick-input.dim {
    width: 9rem;
}

.pick-check {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.pick-warning {
    font-size: var(--text-caption);
    color: var(--node-caution);
}

.pick-hint {
    font-size: var(--text-caption);
    color: var(--text-tertiary);
}

.pick-actions {
    display: flex;
    align-items: center;
    gap: 0.8rem;
}

.pick-note {
    font-size: var(--text-caption);
    color: var(--text-secondary);
    overflow-wrap: anywhere;
}

.project-line {
    display: flex;
    align-items: baseline;
    gap: 0.8rem;
    min-width: 0;
    padding: 0.3rem 0;
}

.project-name {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex: none;
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-weight: 600;
}

.project-line .tag {
    padding: 0.05rem 0.5rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-tertiary);
    background-color: var(--surface-muted);
}

.project-path {
    min-width: 0;
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

/* Process census — one stacked card per process; the column layout is what
   keeps it readable at the pane's ~500px bare-minimum width. */
.proc {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    min-width: 0;
    padding: 0.3rem 0;
}

.proc-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    min-width: 0;
}

.proc-name {
    color: var(--text-primary);
    font-size: var(--text-body-sm);
    font-weight: 600;
}

.proc-tag {
    padding: 0.05rem 0.5rem;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    font-weight: 600;
    color: var(--text-tertiary);
    background-color: var(--surface-muted);
    white-space: nowrap;
}

.proc-tag.badge {
    display: inline-flex;
    align-items: center;
}

.proc-tag.badge .dot {
    width: 0.6rem;
    height: 0.6rem;
    margin-right: 0.4rem;
}

.proc-path {
    min-width: 0;
    font-family: var(--font-mono);
    font-size: var(--text-caption);
    color: var(--text-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.proc-times {
    font-size: var(--text-caption);
    color: var(--text-secondary);
}

.forget {
    flex: none;
    margin-left: auto;
    padding: 0.2rem 0.7rem;
    border-radius: var(--radius-full);
    border: 1px solid var(--border-default);
    background: transparent;
    color: var(--text-tertiary);
    font-size: var(--text-caption);
    cursor: pointer;
}

.forget:hover {
    color: var(--node-problem);
    border-color: color-mix(in srgb, var(--node-problem) 55%, transparent);
}
</style>
