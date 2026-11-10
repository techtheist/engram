<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { withBase } from 'vitepress'
import BrandIcon from './BrandIcon.vue'
import { skin } from './theme'

const INSTALL = 'curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh'

/**
 * The tour: the real pane (the demo build, `?embed`) in a window, driven by
 * this page's scroll. Each chapter id is a scene in frontend/demo/tour.ts;
 * `land` is where a click on its name drops you (progress through it) — past
 * the opening click, where its drawer or menu is already up.
 */
const chapters: { id: string; name: string; text: string; land?: number }[] = [
  {
    id: 'graph',
    name: 'Graph',
    text: 'Every decision, caution and problem your assistant records becomes a note on a graph. Select one to see why it was made, what it is linked to and which session it came from.',
  },
  {
    id: 'feed',
    name: 'Feed',
    text: 'The same memory as a timeline, one card at a time, with version markers between eras.',
  },
  {
    id: 'tasks',
    name: 'Tasks',
    text: 'The assistant’s working list sits beside the graph. Tasks move as the work does, and close on their own at a release, a date or when a note settles.',
  },
  {
    id: 'history',
    name: 'History',
    text: 'Recorded sessions, turn by turn. Every note links back to the conversation it was born in.',
  },
  {
    id: 'review',
    land: 0.3,
    name: 'Review',
    text: 'Contradiction detection: a local model reads new knowledge against what the graph already holds and flags pairs that seem to disagree. Models nominate, people judge: each pair is a conflict, a replacement, or fine together, and nothing changes until someone decides.',
  },
  {
    id: 'checkup',
    land: 0.3,
    name: 'Checkup',
    text: 'Sweeps over the whole graph find hidden conflicts, duplicates, problems that may already be answered and stale notes, on local models. Type a claim and the graph answers with receipts: what supports it, what contradicts it, or silence.',
  },
  {
    id: 'brief',
    land: 0.42,
    name: 'Brief',
    text: 'What the assistant reads when a session starts: the current cycle, open problems, principles and recent decisions — composed from the graph, not written by hand.',
  },
  {
    id: 'projects',
    land: 0.4,
    name: 'Projects',
    text: 'One process serves every project on the machine. Each repository keeps its own graph, and a home graph holds what applies everywhere.',
  },
  {
    id: 'themes',
    land: 0.22,
    name: 'Themes',
    text: 'Engram, Light and Dark. Inside VS Code or a JetBrains IDE the pane takes the IDE’s own theme.',
  },
]

const tour = ref<HTMLElement>()
const chapterList = ref<HTMLElement>()
const viewport = ref<HTMLElement>()
const frame = ref<HTMLIFrameElement>()

const heroP = ref(0)
/** The window rising into place, 0 → 1, before the section pins. */
const rise = ref(0)
/** Progress through the pinned tour, 0 → 1. */
const progress = ref(0)

const index = computed(() => Math.min(chapters.length - 1, Math.floor(progress.value * chapters.length)))
const chapter = computed(() => chapters[index.value])
const within = computed(() => Math.min(1, progress.value * chapters.length - index.value))

// ---- the frame ---------------------------------------------------------------

const load = ref(false)
const ready = ref(false)
/**
 * The pane is laid out at a desktop size and scaled to fit the window — on a
 * phone too: the pane's own narrow layout folds its actions under a menu,
 * and a tour aimed at the desktop one would miss them.
 */
const virtualWidth = 1280
const scale = ref(1)

// The skin is read once per load; later switches go over postMessage, so a
// theme change never reloads the frame.
const src = ref('')
function frameSrc(): string {
  return `${withBase('/demo/')}?embed&theme=${encodeURIComponent(skin.value)}`
}

function post(msg: Record<string, unknown>): void {
  frame.value?.contentWindow?.postMessage({ type: 'engram-tour', ...msg }, location.origin)
}

function sendScene(): void {
  if (!ready.value) return
  // px: one chapter of this page's scroll in the pane's own pixels, so a
  // scroll inside the pane (the feed) moves at the page's pace.
  const el = tour.value
  const px = el ? (el.offsetHeight - innerHeight) / chapters.length / scale.value : 0
  post({
    chapter: progress.value > 0 ? chapter.value.id : 'idle',
    t: Math.round(within.value * 1000) / 1000,
    px,
    scale: scale.value,
  })
}

function onMessage(e: MessageEvent): void {
  if (e.source !== frame.value?.contentWindow || e.data?.type !== 'engram-tour' || !e.data.ready) return
  ready.value = true
  post({ theme: skin.value })
  sendScene()
}

watch([chapter, within], sendScene)

/**
 * On a phone the chapters are a row above the window, wider than the screen:
 * keep the current one centered, its neighbours either side. (On a desktop
 * the list is a column that fits, so this has nothing to scroll.)
 */
function centerChapter(smooth = true): void {
  const list = chapterList.value
  const item = list?.children[index.value] as HTMLElement | undefined
  if (!list || !item) return
  list.scrollTo({
    left: item.offsetLeft - (list.clientWidth - item.offsetWidth) / 2,
    behavior: smooth ? 'smooth' : 'instant',
  })
}
watch(index, () => centerChapter())
watch(skin, (id) => post({ theme: id }))

// ---- scroll -------------------------------------------------------------------

const clamp = (v: number): number => Math.min(1, Math.max(0, v))

let raf = 0
function measure(): void {
  raf = 0
  heroP.value = clamp(scrollY / (innerHeight * 0.7))
  const el = tour.value
  if (!el) return
  const r = el.getBoundingClientRect()
  rise.value = clamp((innerHeight - r.top) / innerHeight)
  progress.value = clamp(-r.top / (r.height - innerHeight))
}

function onScroll(): void {
  if (!raf) raf = requestAnimationFrame(measure)
}

function chapterTop(i: number): number {
  const el = tour.value!
  return el.getBoundingClientRect().top + scrollY + (i / chapters.length) * (el.offsetHeight - innerHeight)
}

/** A chapter name scrolls the page to that chapter's landing point. */
function jump(i: number): void {
  const el = tour.value
  if (!el) return
  const land = chapters[i].land ?? 0.02
  scrollTo({ top: chapterTop(i) + land * ((el.offsetHeight - innerHeight) / chapters.length), behavior: 'smooth' })
}

let resize: ResizeObserver | undefined
let near: IntersectionObserver | undefined

onMounted(() => {
  measure()
  addEventListener('scroll', onScroll, { passive: true })
  addEventListener('resize', onScroll)
  addEventListener('message', onMessage)

  resize = new ResizeObserver(([entry]) => {
    scale.value = entry.contentRect.width / virtualWidth
  })
  if (viewport.value) resize.observe(viewport.value)
  centerChapter(false)

  // The pane loads on its own, after the page: once the tour is a screen
  // away, or when the browser is idle — whichever comes first.
  const start = (): void => {
    if (load.value) return
    src.value = frameSrc()
    load.value = true
    near?.disconnect()
  }
  near = new IntersectionObserver((es) => es.some((e) => e.isIntersecting) && start(), {
    rootMargin: '100% 0px',
  })
  if (tour.value) near.observe(tour.value)
  if ('requestIdleCallback' in window) requestIdleCallback(start, { timeout: 3000 })
  else setTimeout(start, 1500)
})

onBeforeUnmount(() => {
  removeEventListener('scroll', onScroll)
  removeEventListener('resize', onScroll)
  removeEventListener('message', onMessage)
  cancelAnimationFrame(raf)
  resize?.disconnect()
  near?.disconnect()
})

// ---- install ------------------------------------------------------------------

const copied = ref(false)
async function copy(): Promise<void> {
  try {
    await navigator.clipboard.writeText(INSTALL)
    copied.value = true
    setTimeout(() => (copied.value = false), 1600)
  } catch {
    // clipboard refused — the command stays selectable
  }
}
</script>

<template>
  <div class="landing">
    <section class="hero" :style="{ '--p': heroP }">
      <div class="hero-inner">
        <BrandIcon :size="256" class="mark" />
        <h1>Engram Alpha</h1>
        <p class="what">
          Long-term project memory for AI coding assistants — a graph of your project's decisions you can read and edit.
          Runs on your machine.
        </p>
        <nav class="actions">
          <a class="primary" :href="withBase('/docs/getting-started')">Documentation</a>
          <a :href="withBase('/demo/')" target="_self">Demo</a>
          <a href="https://github.com/techtheist/engram">GitHub</a>
        </nav>
        <button type="button" class="install" :class="{ copied }" title="Copy" @click="copy">
          <code>{{ INSTALL }}</code>
          <span class="copy-state">{{ copied ? 'copied' : 'copy' }}</span>
        </button>
      </div>
    </section>

    <section ref="tour" class="tour" :style="{ height: `${chapters.length * 80 + 100}svh` }">
      <div class="stick">
        <div class="stage" :style="{ '--rise': rise }">
          <div class="window">
            <div class="chrome">
              <span class="lights" aria-hidden="true"><i /><i /><i /></span>
              <span class="address">127.0.0.1:8787</span>
            </div>
            <div ref="viewport" class="viewport">
              <iframe
                v-if="load"
                ref="frame"
                :src="src"
                title="Engram Alpha pane, live demo"
                :class="{ shown: ready }"
                :style="{
                  width: `${virtualWidth}px`,
                  height: `${virtualWidth / 1.6}px`,
                  transform: `scale(${scale})`,
                }"
                tabindex="-1"
              />
              <div v-if="!ready" class="placeholder" aria-hidden="true">
                <span class="bar" /><span class="cards"><i /><i /><i /><i /><i /><i /></span>
              </div>
            </div>
          </div>

          <div class="caption" aria-live="polite">
            <Transition name="swap" mode="out-in">
              <div :key="chapter.id">
                <h2><span class="count">{{ index + 1 }} / {{ chapters.length }}</span>{{ chapter.name }}</h2>
                <p>{{ chapter.text }}</p>
              </div>
            </Transition>
          </div>
        </div>

        <ol ref="chapterList" class="chapters">
          <li v-for="(c, i) in chapters" :key="c.id" :class="{ on: i === index, past: i < index }">
            <button type="button" @click="jump(i)">{{ c.name }}</button>
            <span v-if="i === index" class="meter" :style="{ '--t': within }" />
          </li>
        </ol>
      </div>
    </section>

    <section class="end">
      <a class="primary" :href="withBase('/docs/getting-started')">Get started</a>
      <a :href="withBase('/docs/')">All documentation</a>
      <a :href="withBase('/demo/')" target="_self">Demo</a>
      <a :href="withBase('/changelog')">Changelog</a>
    </section>
  </div>
</template>

<style scoped>
.landing {
  --gutter: max(16px, 4vw);
  overflow-x: clip;
  color: var(--text-primary);
}

/* ── Hero ── */
.hero {
  min-height: calc(100svh - var(--vp-nav-height));
  display: grid;
  place-items: center;
  padding: 0 var(--gutter);
}

.hero-inner {
  /* min-width: the unwrapped install line must not widen the grid cell. */
  min-width: 0;
  width: 100%;
  max-width: 760px;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  opacity: calc(1 - var(--p) * 1.2);
  transform: translateY(calc(var(--p) * -60px)) scale(calc(1 - var(--p) * 0.06));
  transform-origin: 50% 0;
}

.mark {
  margin-bottom: 40px;
}

h1 {
  margin: 0;
  font-size: clamp(44px, 9vw, 96px);
  line-height: 1;
  font-weight: 600;
  letter-spacing: -0.035em;
}

.what {
  margin: 24px 0 0;
  max-width: 34em;
  font-size: clamp(16px, 2vw, 19px);
  line-height: 1.55;
  color: var(--text-secondary);
}

.actions,
.end {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 22px;
}

.actions {
  justify-content: center;
  margin-top: 36px;
}

.actions a,
.end a {
  font-size: 15px;
  font-weight: 500;
  color: var(--text-secondary);
  text-decoration: none;
  transition: color 0.2s;
}

.actions a:hover,
.end a:hover {
  color: var(--text-primary);
}

.actions a.primary,
.end a.primary {
  padding: 9px 18px;
  border-radius: 9999px;
  background: var(--interactive-primary);
  color: #fff;
}

.actions a.primary:hover,
.end a.primary:hover {
  background: var(--interactive-primary-hover);
}

.install {
  margin-top: 32px;
  max-width: 100%;
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 10px 14px;
  border: 1px solid var(--border-default);
  border-radius: 8px;
  background: var(--surface-sunken);
  text-align: left;
}

.install code {
  font-family: var(--vp-font-family-mono);
  font-size: 13px;
  color: var(--text-secondary);
  overflow-x: auto;
  white-space: nowrap;
  scrollbar-width: none;
}

.copy-state {
  flex-shrink: 0;
  font-size: 12px;
  color: var(--text-tertiary);
}

.install:hover .copy-state,
.install.copied .copy-state {
  color: var(--interactive-primary);
}

/* ── Tour ── */
.tour {
  /* Rise under the hero: the window is in sight as soon as you scroll. */
  margin-top: -18svh;
}

.stick {
  position: sticky;
  top: var(--vp-nav-height);
  height: calc(100svh - var(--vp-nav-height));
  display: grid;
  grid-template-columns: minmax(0, 1fr) 180px;
  align-items: center;
  gap: clamp(24px, 3vw, 56px);
  padding: 0 var(--gutter);
  max-width: 1440px;
  margin: 0 auto;
}

.stage {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 24px;
}

.window {
  /* As wide as the column allows, as tall as the screen leaves room for. */
  width: min(100%, calc((100svh - var(--vp-nav-height) - 260px) * 1.6));
  border-radius: 12px;
  border: 1px solid var(--border-default);
  background: var(--surface-elevated);
  overflow: hidden;
  box-shadow: 0 24px 60px rgb(0 0 0 / 28%), 0 4px 16px rgb(0 0 0 / 16%);
  /* Fades up into place; no 3D — a live WebGL pane under a perspective
     transform recomposited every frame. */
  opacity: var(--rise);
  transform: translateY(calc((1 - var(--rise)) * 40px));
}

/* Flat chrome: three lights, an address, nothing else to read. */
.chrome {
  display: grid;
  grid-template-columns: 1fr auto 1fr;
  align-items: center;
  height: 36px;
  padding: 0 12px;
  border-bottom: 1px solid var(--border-subtle);
}

.lights {
  display: flex;
  gap: 7px;
}

.lights i {
  width: 11px;
  height: 11px;
  border-radius: 50%;
  background: #ff5f57;
}

.lights i:nth-child(2) {
  background: #febc2e;
}

.lights i:nth-child(3) {
  background: #28c840;
}

.address {
  padding: 2px 32px;
  border-radius: 6px;
  border: 1px solid var(--border-subtle);
  font-family: var(--vp-font-family-mono);
  font-size: 11px;
  color: var(--text-tertiary);
}

.viewport {
  position: relative;
  aspect-ratio: 1.6;
  overflow: hidden;
  background: var(--canvas-bg);
}

.viewport iframe {
  position: absolute;
  top: 0;
  left: 0;
  border: 0;
  transform-origin: 0 0;
  opacity: 0;
  transition: opacity 0.5s;
  /* Watched, not used: the tour drives it, the page scrolls over it, and
     the full demo is a link away for anyone who wants their hands on it. */
  pointer-events: none;
}

.viewport iframe.shown {
  opacity: 1;
}

/* Until the pane is in: its outline, breathing. */
.placeholder {
  position: absolute;
  inset: 0;
  padding: 3%;
  display: flex;
  flex-direction: column;
  gap: 6%;
  animation: breathe 1.6s ease-in-out infinite alternate;
}

.placeholder .bar {
  height: 6%;
  border-radius: 9999px;
  background: var(--surface-muted);
}

.placeholder .cards {
  flex: 1;
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 5% 8%;
  padding: 4% 10%;
}

.placeholder .cards i {
  border-radius: 8px;
  background: var(--surface-muted);
}

@keyframes breathe {
  from {
    opacity: 0.45;
  }

  to {
    opacity: 0.9;
  }
}

.caption {
  width: min(100%, 640px);
  min-height: 132px;
  opacity: var(--rise);
}

.caption h2 {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin: 0 0 8px;
  padding: 0;
  border: 0;
  font-size: 22px;
  font-weight: 600;
  letter-spacing: -0.02em;
}

.caption .count {
  font-size: 12px;
  font-weight: 500;
  color: var(--text-tertiary);
  font-variant-numeric: tabular-nums;
}

.caption p {
  margin: 0;
  font-size: 15px;
  line-height: 1.6;
  color: var(--text-secondary);
}

.swap-enter-active,
.swap-leave-active {
  transition:
    opacity 0.22s,
    transform 0.22s;
}

.swap-enter-from {
  opacity: 0;
  transform: translateY(8px);
}

.swap-leave-to {
  opacity: 0;
  transform: translateY(-8px);
}

/* The chapters, right of the window. */
.chapters {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.chapters li {
  position: relative;
  padding-left: 14px;
}

.chapters li::before {
  content: "";
  position: absolute;
  left: 0;
  top: 8px;
  bottom: 8px;
  width: 2px;
  border-radius: 1px;
  background: var(--border-subtle);
}

.chapters .meter {
  position: absolute;
  left: 0;
  top: 8px;
  width: 2px;
  height: calc((100% - 16px) * var(--t));
  border-radius: 1px;
  background: var(--interactive-primary);
}

.chapters button {
  padding: 6px 0;
  font-size: 15px;
  font-weight: 500;
  color: var(--text-tertiary);
  text-align: left;
  transition: color 0.2s;
}

.chapters button:hover,
.chapters .past button {
  color: var(--text-secondary);
}

.chapters .on button {
  color: var(--text-primary);
}

/* ── End ── */
.end {
  justify-content: center;
  padding: 12svh var(--gutter) 18svh;
}

@media (width <= 900px) {
  .stick {
    grid-template-columns: minmax(0, 1fr);
    align-content: center;
    gap: 20px;
  }

  .window {
    width: 100%;
  }

  /* The chapters become a row above the window, scrolled so the current one
     sits in the middle; the edges fade where the row runs off-screen. */
  .chapters {
    order: -1;
    flex-direction: row;
    gap: 22px;
    overflow-x: auto;
    scrollbar-width: none;
    padding: 0 40%;
    mask-image: linear-gradient(90deg, transparent, #000 22%, #000 78%, transparent);
  }

  .chapters::-webkit-scrollbar {
    display: none;
  }

  .chapters li {
    flex-shrink: 0;
    padding: 0 0 6px;
  }

  .chapters li::before {
    inset: auto 0 0;
    width: auto;
    height: 2px;
  }

  .chapters .meter {
    inset: auto auto 0 0;
    width: calc(100% * var(--t));
    height: 2px;
  }

  .chapters button {
    white-space: nowrap;
  }
}

@media (prefers-reduced-motion: reduce) {
  .hero-inner,
  .window {
    transform: none !important;
  }

  .hero-inner {
    opacity: 1 !important;
  }
}
</style>
