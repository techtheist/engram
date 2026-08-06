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
import type { AnsweredHint, ClaimReport, DriftEntry, FsListing, ProjectInfo, SearchHit } from '@/types/graph'
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
