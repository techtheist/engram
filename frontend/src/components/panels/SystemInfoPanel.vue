<script setup lang="ts">
import { ref, watch } from 'vue'
import SidePanel from '@/components/common/SidePanel.vue'
import { api } from '@/services/api'
import { useSystemInfo } from '@/composables/useSystemInfo'
import type { SystemInfo } from '@/types/graph'

/**
 * Settings → System info: the daemon-side half of `engram-alpha doctor`,
 * formatted — binary version, store health, embedding model, and which
 * assistants are wired to this repo (GET /system).
 */
const { open, hide } = useSystemInfo()

const info = ref<SystemInfo | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)

watch(open, (isOpen) => {
    if (isOpen) void reload()
})

async function reload(): Promise<void> {
    loading.value = true
    error.value = null
    try {
        info.value = await api.system()
    } catch (e) {
        error.value = e instanceof Error ? e.message : String(e)
        info.value = null
    } finally {
        loading.value = false
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

        <section class="block">
            <h3 class="block-title">Graph store</h3>
            <dl class="rows">
                <div><dt>Database</dt><dd class="mono">{{ info.store.db ?? '—' }}</dd></div>
                <div><dt>Size</dt><dd>{{ fmtBytes(info.store.size_bytes) }}</dd></div>
                <div><dt>Contents</dt><dd>{{ info.store.nodes }} nodes · {{ info.store.edges }} edges</dd></div>
                <div>
                    <dt>Integrity</dt>
                    <dd>
                        <span class="dot" :data-status="info.store.integrity_ok ? 'ok' : 'bad'" />
                        {{ info.store.integrity_ok ? 'quick_check passed' : 'quick_check FAILED' }}
                    </dd>
                </div>
                <div>
                    <dt>Journal</dt>
                    <dd>
                        <span class="dot" :data-status="info.store.journal_mode === 'wal' ? 'ok' : 'warn'" />
                        {{ info.store.journal_mode }}
                    </dd>
                </div>
                <div>
                    <dt>Vectors</dt>
                    <dd>
                        <span class="dot" :data-status="embeddingStatus(info).status" />
                        {{ embeddingStatus(info).text }} · {{ info.store.fts }} in the FTS index
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
            <h3 class="block-title">Embedding model</h3>
            <dl class="rows">
                <div>
                    <dt>Local cache</dt>
                    <dd>
                        <span class="dot" :data-status="info.model_cached ? 'ok' : 'warn'" />
                        {{ info.model_cached ? 'downloaded (~/.cache/engram)' : 'not downloaded yet — first real-embedding run fetches it (~30 MB)' }}
                    </dd>
                </div>
            </dl>
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
</style>
