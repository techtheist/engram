/**
 * The pane's backend, whichever one it is.
 *
 * Everything imports `@/services/api`; nothing imports an implementation
 * directly. The normal build resolves this to the HTTP client below. The
 * demo build (`ENGRAM_DEMO=1`, see `vite.config.ts`) aliases this very
 * specifier to `frontend/demo/api.ts` — a mock daemon that runs entirely in
 * the browser tab. Neither build carries a byte of the other.
 */
export * from '@/services/http'
