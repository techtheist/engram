/**
 * The payloads no browser can compute.
 *
 * Search ranking, claim checking and the Checkup sweeps are the local
 * cortex's work — an embedding model, a cross-encoder reranker and an NLI
 * model, none of which are going to be downloaded into a demo tab. So these
 * are fixed, hand-written responses, shaped exactly like the daemon's, and
 * the demo banner says plainly that they are pre-recorded rather than
 * dressing up a keyword match as semantic recall.
 *
 * The hits are real nodes from the demo graph, so selecting one focuses the
 * node it names, and what they say about how the memory works is true of the
 * shipped product.
 */
import type {
    AnsweredHint,
    BornIn,
    ClaimReport,
    DriftEntry,
    FsListing,
    HistoryMessage,
    HistorySession,
    ProjectInfo,
    SearchHit,
} from '@/types/graph'
import { synthId } from '../engine'

/** The backend marks keyword matches with these; SearchBar turns them into <mark>. */
const HI = '\uE000'
const OFF = '\uE001'

/**
 * Four results, returned for every query. Together they answer "what does
 * this thing actually remember, and why should I trust it?" — which is the
 * question a visitor typing into the search box is really asking.
 */
export const SEARCH_HITS: SearchHit[] = [
    {
        id: synthId('p-research-first'),
        type: 'Principle',
        title: 'Research-first: a retrieval change cites a measured run or it does not ship',
        snippet: `Ranking, recall and relevance work is judged by the ${HI}eval${OFF} harness, never by how the diff reads or how one query felt in the demo library. A change with no measured before/after is a hypothesis, and hypotheses live in Intents — not in main.`,
        score: 0.94,
        durability: 'stable',
        status: null,
    },
    {
        id: synthId('i-stable-doesnt-rot'),
        type: 'Insight',
        title: 'Trust that decays with time punished notes that were simply true and old',
        snippet: `When every note faded on a clock, "SQLite is the only store" was quietly rated less trustworthy than a three-week-old guess. ${HI}Durability${OFF} is what separates them: stable knowledge holds flat until evidence moves it, while episodic notes genuinely do expire.`,
        score: 0.81,
        durability: 'episodic',
        status: null,
    },
    {
        id: synthId('c-conflicts-surfaced'),
        type: 'Caution',
        title: 'A conflict is surfaced, never auto-resolved',
        snippet: `The scan finds candidate pairs; a person judges them. Two notes that genuinely disagree are information, and the failure mode of resolving them automatically is that the graph looks ${HI}consistent${OFF} while quietly holding the wrong half.`,
        score: 0.76,
        durability: 'stable',
        status: null,
    },
    {
        id: synthId('d-on-device-search'),
        type: 'Decision',
        title: 'Semantic search runs on-device with a small encoder — no cloud calls, ever',
        snippet: `A 384-dim encoder (~40 MB quantized) embeds titles, chapter headings and highlight text at import. Hosted embeddings are off the table entirely, because sending a library's contents to an API is the one thing the whole product ${HI}promises${OFF} not to do.`,
        score: 0.71,
        durability: 'stable',
        status: null,
    },
]

/** One pre-recorded claim check, echoing whatever claim was typed. */
export function claimReport(claim: string): ClaimReport {
    return {
        claim,
        supports: [
            {
                id: synthId('d-on-device-search'),
                type: 'Decision',
                title: 'Semantic search runs on-device with a small encoder — no cloud calls, ever',
                trust: 0.6,
                stale: false,
                entailment: 0.91,
                neutral: 0.07,
                contradiction: 0.02,
            },
            {
                id: synthId('p-local-first'),
                type: 'Principle',
                title: 'A library never leaves the machine without an explicit gesture',
                trust: 1,
                stale: false,
                entailment: 0.78,
                neutral: 0.2,
                contradiction: 0.02,
            },
        ],
        contradicts: [
            {
                id: synthId('d-paginate'),
                type: 'Decision',
                title: 'EPUB rendering paginates on a canvas — no scrolling view',
                trust: 0.5,
                stale: false,
                entailment: 0.04,
                neutral: 0.11,
                contradiction: 0.85,
            },
        ],
        silent: [
            {
                id: synthId('c-webview'),
                type: 'Caution',
                title: 'The system WebView differs per platform — test pagination on WebKitGTK before every release',
                trust: 0.6,
                stale: false,
                entailment: 0.12,
                neutral: 0.83,
                contradiction: 0.05,
            },
        ],
    }
}

/** An open Problem an existing note may already answer. */
export const ANSWERED_HINTS: AnsweredHint[] = [
    {
        problem: {
            id: synthId('pr-cold-start'),
            type: 'Problem',
            title: 'Cold-start search on a 4,000-book library takes about six seconds',
        },
        candidate: {
            id: synthId('i-cold-start-is-index'),
            type: 'Insight',
            title: 'The six-second cold start is index load, not search — the encoder is 400 ms of it',
        },
        entailment: 0.88,
        existing_link: 'builds-on',
    },
    {
        problem: {
            id: synthId('pr-anchor-drift'),
            type: 'Problem',
            title: 'Highlights land in the wrong paragraph after a book is re-imported',
        },
        candidate: {
            id: synthId('i-anchoring-not-sync'),
            type: 'Insight',
            title: 'Highlight drift is an anchoring problem, not a sync problem',
        },
        entailment: 0.83,
    },
]

/** Notes whose code_refs no longer resolve — the 0.5 refactor moved two files. */
export const DRIFT: DriftEntry[] = [
    {
        id: synthId('a-anno'),
        type: 'Anchor',
        title: 'Annotation store — highlights and notes',
        missing: ['src-tauri/src/anno/anchor.rs'],
    },
    {
        id: synthId('d-pdf-text'),
        type: 'Decision',
        title: 'PDF text layers are extracted once at import and cached in the index',
        missing: ['src-tauri/src/index/pdf_text.rs'],
    },
]

export const PROJECTS: ProjectInfo[] = [
    {
        id: 'lantern',
        name: 'lantern',
        root: '/Users/you/code/lantern',
        db: '/Users/you/code/lantern/.engram/graph.tepin',
        current: true,
        home: false,
        open: true,
    },
    {
        id: 'home',
        name: 'home',
        db: '/Users/you/.engram/home.tepin',
        current: false,
        home: true,
        open: true,
    },
]

export const FS_LISTING: FsListing = {
    path: '/Users/you/code',
    parent: '/Users/you',
    home: '/Users/you',
    dirs: [
        { name: 'lantern', path: '/Users/you/code/lantern', engram: true, git: true },
        { name: 'atlas', path: '/Users/you/code/atlas', engram: true, git: true },
        { name: 'scratch', path: '/Users/you/code/scratch', engram: false, git: false },
        { name: 'vendor', path: '/Users/you/code/vendor', engram: false, git: true },
    ],
}

// ---- session history (0.8.4) ----------------------------------------------

const now = Math.floor(Date.now() / 1000)
const daysAgo = (d: number, offsetMin = 0) => now - d * 86_400 + offsetMin * 60

/**
 * Six recorded Lantern sessions — the raw dialogue the whole curated graph
 * was born in, one per era of the project (kickoff → sync design → the
 * search/retro session → the bug storm → profiling on Codex → the beta
 * readout). Ages are relative like the seed graph, so the demo never looks
 * stale, and every seed note carries a born-in link into one of these
 * exchanges (see BORN_IN below).
 */
export const HISTORY_SESSIONS: HistorySession[] = [
    {
        node_id: synthId('hs-beta-session'),
        session: 'lantern-beta-session',
        title: 'beta readout is in — 40 reading sessions. what did we learn?',
        harness: 'claude-code',
        started: daysAgo(9),
        ended: daysAgo(9, 29),
        messages: 6,
        version: '0.5.0',
    },
    {
        node_id: synthId('hs-profiling-session'),
        session: 'lantern-profiling-session',
        title: 'cold start is six seconds on my 4,000-book library — profile it before…',
        harness: 'codex',
        started: daysAgo(26),
        ended: daysAgo(26, 41),
        messages: 6,
        version: '0.5.0',
    },
    {
        node_id: synthId('hs-storm-session'),
        session: 'lantern-storm-session',
        title: 'two fires this week: resize flickers, and every launch re-uploads the…',
        harness: 'claude-code',
        started: daysAgo(106),
        ended: daysAgo(106, 35),
        messages: 4,
        version: '0.4.0',
    },
    {
        node_id: synthId('hs-search-session'),
        session: 'lantern-search-session',
        title: 'can we do semantic search without sending anything to a server?',
        harness: 'claude-code',
        started: daysAgo(120),
        ended: daysAgo(120, 58),
        messages: 8,
        version: '0.4.0',
    },
    {
        node_id: synthId('hs-sync-session'),
        session: 'lantern-sync-session',
        title: 'sync design — I refuse to run servers for this',
        harness: 'claude-code',
        started: daysAgo(158),
        ended: daysAgo(158, 53),
        messages: 6,
        version: '0.3.0',
    },
    {
        node_id: synthId('hs-kickoff-session'),
        session: 'lantern-kickoff-session',
        title: 'starting Lantern — a local-first reading app. before any code: the shell…',
        harness: 'claude-code',
        started: daysAgo(170),
        ended: daysAgo(170, 47),
        messages: 6,
        version: '0.3.0',
    },
]

const msg = (
    key: string,
    role: 'user' | 'assistant',
    turn: number,
    timestamp: number,
    text: string,
): HistoryMessage => ({ message_id: synthId(key), role, turn, timestamp, text })

export const HISTORY_MESSAGES: Record<string, HistoryMessage[]> = {
    'lantern-kickoff-session': [
        msg('hm-ko-0', 'user', 0, daysAgo(170),
            'starting Lantern — a local-first reading app. before any code: the shell, the store, the reading surface, and the promise we can\'t break.'),
        msg('hm-ko-1', 'assistant', 1, daysAgo(170, 4),
            'Proposal: **Tauri** over Electron (a 6 MB binary against 180, and the Rust core we want anyway), **bundled SQLite** as the only store — one file the user can copy and delete, no daemon — and **canvas pagination** with cached layout, because page-anchored highlights need pages. That gives us four subsystems to hang knowledge off: the reader canvas, the sync engine, the library index and the annotation store. Registering all four as anchors and capturing the three decisions now.'),
        msg('hm-ko-2', 'user', 2, daysAgo(170, 25),
            'agreed on all of it. the privacy line is absolute — a library never leaves the machine, offline is ordinary life, not an error state. and I want keyboard bindings before buttons, always.'),
        msg('hm-ko-3', 'assistant', 3, daysAgo(170, 28),
            'Captured as canon: the library never leaves the machine without an explicit gesture (pinned — it\'s the product), offline is the default state, and keyboard-first — every reading action gets a binding before it gets a button.'),
        msg('hm-ko-4', 'user', 4, daysAgo(170, 44),
            'two scars from the last reader I built: keep a fixture library that reproduces every layout bug we hit, and store highlights as text offsets — page numbers die on the first font-size change.'),
        msg('hm-ko-5', 'assistant', 5, daysAgo(170, 47),
            'Both kept: the twelve-book demo library goes in as the fixture set (one RTL title, one with SVG chapter art, one 1,400-page reference, one with no chapter marks), and the caution is recorded — page numbers are not anchors, highlights live as text offsets into the paragraph.'),
    ],
    'lantern-sync-session': [
        msg('hm-sy-0', 'user', 0, daysAgo(158),
            'sync. I refuse to run servers for this — what are our options?'),
        msg('hm-sy-1', 'assistant', 1, daysAgo(158, 3),
            'Then the honest option is folder replication over storage the user already owns: Lantern writes annotation files into the library folder and Dropbox, iCloud or Syncthing move the bytes. We hold no keys and run nothing — the cost is inheriting whatever partial-write behaviour the drive has.'),
        msg('hm-sy-2', 'user', 2, daysAgo(158, 25),
            'acceptable cost. take it.'),
        msg('hm-sy-3', 'assistant', 3, daysAgo(158, 26),
            'Captured as a decision — and collisions resolve last-writer-wins for the first release: compare mtimes, newest file wins the book. Simplest thing that can work; it is on record so it can be replaced on evidence instead of forgotten.'),
        msg('hm-sy-4', 'user', 4, daysAgo(158, 50),
            'one thing that scares me: cloud drives hand you half a file. and someone will eventually point the library at a NAS.'),
        msg('hm-sy-5', 'assistant', 5, daysAgo(158, 53),
            'Both recorded as cautions. The watcher never reads on the create event — Dropbox and iCloud make the inode before the content is complete, so it debounces, verifies the hash, then reads. And the index refuses to open a library on SMB/NFS: SQLite WAL over network locking corrupts quietly, so we fail loudly with an explanation instead.'),
    ],
    'lantern-search-session': [
        msg('hm-se-0', 'user', 0, daysAgo(120),
            'can we do semantic search without sending anything to a server? that promise is the whole product.'),
        msg('hm-se-1', 'assistant', 1, daysAgo(120, 2),
            'Yes — a small on-device encoder. A 384-dim model is ~40 MB quantized and embeds titles, chapter headings and highlight text at import time. The trade is recall against the hosted giants, so let me benchmark the candidates on your library first.'),
        msg('hm-se-2', 'user', 2, daysAgo(120, 35),
            'benchmarks look fine to me. lock it in: on-device only, no cloud calls, ever.'),
        msg('hm-se-3', 'assistant', 3, daysAgo(120, 36),
            'Locked, and the measurement is on record: the encoder costs 40 MB and buys 0.18 recall over keyword search alone — hybrid wins at every cutoff, keywords stay for exact-title lookups. Hosted embeddings are off the table entirely, because sending a library\'s contents to an API is the one thing Lantern promises not to do.'),
        msg('hm-se-4', 'user', 4, daysAgo(120, 45),
            'while the eval set runs — a retro on how we keep this project\'s memory. write the rules down: you propose and I judge; being retrieved proves nothing; retired means retired; conflicts get surfaced, never auto-resolved; search before you write; and stamp every decision with the version it was made under.'),
        msg('hm-se-5', 'assistant', 5, daysAgo(120, 49),
            'Captured the working rules as canon: models nominate and people judge (so deletion has no assistant verb at all), trust reads deliberate acts only, a superseded decision stays in history but never in canon, conflicts wait for a human, every capture starts with a search and ends by reading the verdict, versions stamp every decision, and every edge is a sentence. Kept the eval habit itself as a principle too — a retrieval change cites a measured run or it does not ship. One more from the trust rework: time-decay was punishing notes that were simply true and old; durability is the axis that separates them.'),
        msg('hm-se-6', 'user', 6, daysAgo(120, 55),
            'last thing — the annotation merge. last-writer-wins ate a chapter of highlights when two devices came back online after a week.'),
        msg('hm-se-7', 'assistant', 7, daysAgo(120, 58),
            'Redesigned: highlights key on their paragraph anchor and merge as a per-paragraph grow-only CRDT — two devices annotating different chapters stop eating each other\'s work. The last-writer-wins decision retires with the story of why attached.'),
    ],
    'lantern-storm-session': [
        msg('hm-st-0', 'user', 0, daysAgo(106),
            'two fires this week: resize flickers and loses my reading position, and every launch makes the cloud drive re-upload the whole library.'),
        msg('hm-st-1', 'assistant', 1, daysAgo(106, 5),
            'Diagnosed both. Resize triggers a full relayout per event and restores position from a page number that no longer means the same place — and while I was in the reader, the one-line page shifts people report trace to WebKitGTK rounding column widths differently, so that goes on record as a per-release test caution. The re-upload storm is unstable serialization: key order changes between runs, identical content produces different bytes, the drive sees thousands of modified files.'),
        msg('hm-st-2', 'user', 2, daysAgo(106, 32),
            'fixes look right, ship both. and while I remember — reading stats stay opt-in, off by default, computed locally.'),
        msg('hm-st-3', 'assistant', 3, daysAgo(106, 35),
            'Shipped and captured: resize debounced to 120 ms with the layout cache reused and the position restored from a text offset; annotation files written only when the content hash changes, with sorted keys — launch now writes zero files on an unchanged library. Worth keeping: the truncated-JSON crash and the vanished-annotation report were the same partial-file bug in different clothes. Stats decision recorded — opt-in, off by default, nothing transmitted either way.'),
    ],
    'lantern-profiling-session': [
        msg('hm-pf-0', 'user', 0, daysAgo(26),
            'cold start is six seconds on my 4,000-book library — profile it before guessing. also: highlights drift after re-import, and we just swapped the encoder, so every old recall number is suspect.'),
        msg('hm-pf-1', 'assistant', 1, daysAgo(26, 6),
            'Profiled the first query: 5.1 s is the vector index loading off disk, 0.4 s the encoder, 0.5 s everything else — it is index load, not search, and optimising the encoder would buy almost nothing. Queued memory-mapping the index. Related and settled while I was there: PDF text layers extract once at import into a hash-keyed cache, never on the query path.'),
        msg('hm-pf-2', 'user', 2, daysAgo(26, 22),
            'good. now the drift?'),
        msg('hm-pf-3', 'assistant', 3, daysAgo(26, 26),
            'It reproduces with sync switched off entirely — re-import alone moves the highlights, so it is an anchoring problem, not a sync problem; every hour spent in the merge code was spent in the wrong file. Problem filed with the repro, and the WebKitGTK pagination matrix is queued for CI while the reader is open on the bench.'),
        msg('hm-pf-4', 'user', 4, daysAgo(26, 38),
            'and clean up after the encoder swap — what does the graph need to learn from this?'),
        msg('hm-pf-5', 'assistant', 5, daysAgo(26, 41),
            'Three captures. A benchmark number does not survive a model swap — any figure quoted in a note carries the model it was measured with, or it is not quoted. Every note we ever duplicated was one nobody searched for before writing the second copy — a habit problem, not a retrieval one. And the annotation merge grows its missing half: overlapping highlights on the same paragraph now surface a conflict card in the reader instead of merging into soup — that decision replaces the silent grow-only merge.'),
    ],
    'lantern-beta-session': [
        msg('hm-be-0', 'user', 0, daysAgo(9),
            'beta readout is in — 40 reading sessions. what did we learn?'),
        msg('hm-be-1', 'assistant', 1, daysAgo(9, 5),
            'The reading mode is a device fact, not a preference: nobody switched it once set, and the split is almost perfectly phones-scroll, desktops-paginate. Recorded — and it now sits in open conflict with the pagination-only decision. Surfaced for you to judge; I do not resolve those.'),
        msg('hm-be-2', 'user', 2, daysAgo(9, 15),
            'ha. and the stats panel?'),
        msg('hm-be-3', 'assistant', 3, daysAgo(9, 18),
            'Three percent opted in — enough data to describe about thirty people, or more precisely the kind of person who enables stats panels. Recorded as an insight, and it queues a second conflict against the opt-in-by-default decision: the privacy call and the feature\'s usefulness are now openly at odds.'),
        msg('hm-be-4', 'user', 4, daysAgo(9, 27),
            'queue the next moves: recall gets re-measured before anything ships, and the conflict card goes out this cycle.'),
        msg('hm-be-5', 'assistant', 5, daysAgo(9, 29),
            'Both on the worklist: the re-measure intent waits on the research-first rule, the conflict-card intent on the merge decision.'),
    ],
}

/**
 * Which curated note was born in which recorded exchange — every seed note
 * has one, so the drawer's Session chip always lands somewhere real.
 * Shape per row: [note key, session, message key, day, minute offset].
 */
const BIRTHS: Array<[string, string, string, number, number]> = [
    // kickoff — the architecture litter (one assistant message, many notes)
    ['d-tauri', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['d-sqlite', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['d-paginate', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['a-reader', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['a-sync', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['a-index', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['a-anno', 'lantern-kickoff-session', 'hm-ko-1', 170, 4],
    ['p-local-first', 'lantern-kickoff-session', 'hm-ko-3', 170, 28],
    ['p-offline-default', 'lantern-kickoff-session', 'hm-ko-3', 170, 28],
    ['d-keyboard-first', 'lantern-kickoff-session', 'hm-ko-3', 170, 28],
    ['i-demo-library', 'lantern-kickoff-session', 'hm-ko-5', 170, 47],
    ['c-anchors-offsets', 'lantern-kickoff-session', 'hm-ko-5', 170, 47],
    // sync design
    ['d-folder-sync', 'lantern-sync-session', 'hm-sy-1', 158, 3],
    ['d-anno-lww', 'lantern-sync-session', 'hm-sy-3', 158, 26],
    ['c-partial-files', 'lantern-sync-session', 'hm-sy-5', 158, 53],
    ['c-wal-network', 'lantern-sync-session', 'hm-sy-5', 158, 53],
    // search + the memory-practice retro
    ['d-on-device-search', 'lantern-search-session', 'hm-se-1', 120, 2],
    ['i-embed-measured', 'lantern-search-session', 'hm-se-3', 120, 36],
    ['p-research-first', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['p-nominate-judge', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['p-graph-of-record', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['p-trust-deliberate', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['p-edges-sentences', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['d-user-delete', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['d-search-before-write', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['d-version-stamp', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['c-retired-means-retired', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['c-conflicts-surfaced', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['i-stable-doesnt-rot', 'lantern-search-session', 'hm-se-5', 120, 49],
    ['d-anno-crdt', 'lantern-search-session', 'hm-se-7', 120, 58],
    // the bug storm
    ['pr-resize-flicker', 'lantern-storm-session', 'hm-st-1', 106, 5],
    ['pr-sync-rewrite', 'lantern-storm-session', 'hm-st-1', 106, 5],
    ['c-webview', 'lantern-storm-session', 'hm-st-1', 106, 5],
    ['r-resize', 'lantern-storm-session', 'hm-st-3', 106, 35],
    ['r-sync-hash', 'lantern-storm-session', 'hm-st-3', 106, 35],
    ['i-same-bug', 'lantern-storm-session', 'hm-st-3', 106, 35],
    ['d-stats-optin', 'lantern-storm-session', 'hm-st-3', 106, 35],
    // profiling (codex)
    ['pr-cold-start', 'lantern-profiling-session', 'hm-pf-1', 26, 6],
    ['i-cold-start-is-index', 'lantern-profiling-session', 'hm-pf-1', 26, 6],
    ['n-mmap-index', 'lantern-profiling-session', 'hm-pf-1', 26, 6],
    ['d-pdf-text', 'lantern-profiling-session', 'hm-pf-1', 26, 6],
    ['pr-anchor-drift', 'lantern-profiling-session', 'hm-pf-3', 26, 26],
    ['i-anchoring-not-sync', 'lantern-profiling-session', 'hm-pf-3', 26, 26],
    ['n-webkit-matrix', 'lantern-profiling-session', 'hm-pf-3', 26, 26],
    ['c-stale-benchmark', 'lantern-profiling-session', 'hm-pf-5', 26, 41],
    ['i-nobody-searched', 'lantern-profiling-session', 'hm-pf-5', 26, 41],
    ['d-anno-crdt-cards', 'lantern-profiling-session', 'hm-pf-5', 26, 41],
    // beta readout
    ['i-device-mode', 'lantern-beta-session', 'hm-be-1', 9, 5],
    ['i-stats-coverage', 'lantern-beta-session', 'hm-be-3', 9, 18],
    ['n-remeasure-recall', 'lantern-beta-session', 'hm-be-5', 9, 29],
    ['n-conflict-card', 'lantern-beta-session', 'hm-be-5', 9, 29],
]

export const BORN_IN: Record<string, BornIn> = Object.fromEntries(
    BIRTHS.map(([note, session, message, day, offset]) => [
        synthId(note),
        { session, message_id: synthId(message), timestamp: daysAgo(day, offset), turn: Number(message.split('-').pop()) },
    ]),
)
