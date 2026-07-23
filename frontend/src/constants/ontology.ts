/*
 * Since 0.7 the ontology (type names, verbs, colors) is per-graph data —
 * everything that used to be hardcoded here lives in the config store
 * (`stores/config.ts`), derived from GET /config. Only ontology-independent
 * UI constants remain.
 */

/**
 * Recommended starting tags (PLAN §10): offered in the editor before the graph
 * has grown its own vocabulary; real usage takes over from there.
 */
export const DEFAULT_TAGS: string[] = ['tech-decision', 'preference', 'process', 'domain']

/** Backend trust at/above which a node reads as "trusted" in the UI. */
export const TRUSTED_TRUST = 0.7
