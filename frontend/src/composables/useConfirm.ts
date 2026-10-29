import { shallowRef } from 'vue'

/**
 * `await confirmDialog({...})` — the pane's window.confirm (0.10.3). IDE
 * webviews sandbox the native one into a silent `false`, so every confirm
 * goes through one ConfirmHost mounted in App. One question at a time: a new
 * one answers the pending one with `false`.
 */
export interface ConfirmOptions {
    title: string
    message: string
    confirmLabel?: string
    cancelLabel?: string
    /** Destructive: danger tint, and focus starts on Cancel. */
    danger?: boolean
}

interface Pending extends ConfirmOptions {
    resolve: (ok: boolean) => void
}

const pending = shallowRef<Pending | null>(null)

export function confirmDialog(options: ConfirmOptions): Promise<boolean> {
    pending.value?.resolve(false)
    return new Promise((resolve) => {
        pending.value = { ...options, resolve }
    })
}

/** ConfirmHost's side: the open question and how to answer it. */
export function useConfirmState() {
    function settle(ok: boolean): void {
        const p = pending.value
        pending.value = null
        p?.resolve(ok)
    }
    return { pending, settle }
}
