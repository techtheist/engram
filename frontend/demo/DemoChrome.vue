<script setup lang="ts">
/**
 * The demo's only piece of extra UI: a corner badge that says what this is,
 * what is real about it and what is faked, plus the reset. Mounted through
 * `services/hostChrome` so App.vue stays unaware a demo exists.
 */
import { ref } from 'vue'
import { resetWorld } from './engine'

const open = ref(false)

function reset(): void {
    resetWorld()
    // Cleanest possible restart: every store re-reads the rebuilt graph.
    location.reload()
}
</script>

<template>
<div class="demo-chrome" :class="{ open }">
    <button class="badge" type="button" @click="open = !open">
        <span class="dot" />
        <span>Demo</span>
    </button>

    <div v-if="open" class="card glass-panel">
        <h2>You are inside the real pane</h2>
        <p>
            This is Engram Alpha's graph pane, built from the same source that ships in the
            daemon, the VS&nbsp;Code extension and the JetBrains plugin. Behind it, instead of the
            local daemon, is a small memory engine running in this tab over the invented memory of
            a fictional project.
        </p>
        <p class="real">
            <strong>Everything editable is genuinely editable.</strong> Create notes, rewrite them,
            drag links between them, approve, pin, judge a suspected conflict, run the decay
            preview, retype the whole ontology. Trust is computed the way the daemon computes it,
            so approving a note or pinning it moves the number for the same reasons.
        </p>
        <p class="faked">
            Search and claim checks return <em>pre-recorded</em> results: those need the local
            models — an embedding encoder, a cross-encoder reranker and an NLI model — and none of
            them belong in a browser tab. Anything touching a real filesystem (registering a
            project, installing the skill, swapping a model) politely refuses.
        </p>
        <p class="scope">
            Your edits live in this tab only. A reload keeps them; closing the tab throws them
            away. Nothing is uploaded, because there is nowhere to upload it to.
        </p>
        <div class="actions">
            <button class="reset" type="button" @click="reset">Reset the demo graph</button>
            <a class="link" href="https://github.com/techtheist/engram" target="_blank" rel="noopener">
                GitHub&nbsp;↗
            </a>
        </div>
    </div>
</div>
</template>

<style scoped>
.demo-chrome {
    position: absolute;
    right: 1.6rem;
    /* Clear of the canvas minimap, which owns the bottom-right corner. */
    bottom: 10rem;
    z-index: 9;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.8rem;
    font-family: var(--font-sans);
}

.badge {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 1.2rem;
    border: 1px solid var(--border-default);
    border-radius: var(--radius-full);
    background-color: var(--surface-glass);
    backdrop-filter: var(--glass-backdrop);
    color: var(--text-secondary);
    font-size: var(--text-label);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    cursor: pointer;
}

.badge:hover {
    color: var(--text-primary);
}

.dot {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: var(--radius-full);
    background-color: var(--interactive-primary);
}

.card {
    order: -1;
    width: min(38rem, calc(100vw - 3.2rem));
    padding: 1.6rem;
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    color: var(--text-secondary);
    font-size: var(--text-body-sm);
    line-height: 1.55;
}

.card h2 {
    margin-bottom: 0.8rem;
    color: var(--text-primary);
    font-size: var(--text-body);
    font-weight: 600;
}

.card p {
    margin-bottom: 0.8rem;
}

.card p:last-of-type {
    margin-bottom: 1.2rem;
}

.real strong {
    color: var(--text-primary);
}

.faked,
.scope {
    color: var(--text-tertiary);
}

.actions {
    display: flex;
    align-items: center;
    gap: 1.2rem;
}

.reset {
    padding: 0.6rem 1.2rem;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background-color: var(--interactive-primary);
    color: var(--text-inverse);
    font-size: var(--text-label);
    font-weight: 600;
    cursor: pointer;
}

.reset:hover {
    background-color: var(--interactive-primary-hover);
}

.link {
    color: var(--text-secondary);
    font-size: var(--text-label);
    text-decoration: none;
}

.link:hover {
    color: var(--text-primary);
}

/* Narrow panes stack the health strip under the minimap — move further up. */
@media (width <= 700px) {
    .demo-chrome {
        bottom: 12rem;
    }
}
</style>
