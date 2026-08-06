/**
 * The whole backend surface the pane is allowed to touch.
 *
 * The HTTP client (`services/http.ts`) DEFINES it — every other
 * implementation must conform. Today there is one other: the browser-only
 * mock under `frontend/demo/`, which powers the GitHub Pages demo. That is
 * the point of naming the type: add a method to the client and the demo
 * stops compiling until it answers for it too, so the demo can't quietly rot
 * into a different product than the one that ships.
 */
import type { api } from '@/services/http'

/** Callbacks for the live-change stream (SSE in the daemon, an in-page emitter in the demo). */
export interface StreamHandlers {
    /** One raw JSON frame, exactly as the daemon writes it. */
    message: (raw: string) => void
    open: () => void
    error: () => void
}

export type EngramApi = typeof api
