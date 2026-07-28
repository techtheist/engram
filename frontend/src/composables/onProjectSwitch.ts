import { watch } from 'vue'
import { useProjectsStore } from '@/stores/projects'

/**
 * Run `reset` after every completed project switch. Panels that survive the
 * switch (they are only hidden, never unmounted) hold state that describes
 * ONE graph — a search query and its hits, a sweep report, a traversal trail
 * — and showing it under the next project presents the old graph's truth as
 * the new one's. Nothing is restored on the way back: a switch starts clean.
 */
export function onProjectSwitch(reset: () => void): void {
    const projects = useProjectsStore()
    watch(() => projects.switchEpoch, reset)
}
