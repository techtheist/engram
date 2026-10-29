<script setup lang="ts">
import ModalDialog from '@/components/common/ModalDialog.vue'
import UiButton from '@/components/common/UiButton.vue'
import { useConfirmState } from '@/composables/useConfirm'

/** Renders whatever `confirmDialog()` is asking; mounted once, in App. */
const { pending, settle } = useConfirmState()
</script>

<template>
<ModalDialog
    v-if="pending"
    :title="pending.title"
    :tone="pending.danger ? 'danger' : 'default'"
    @close="settle(false)"
>
    <p class="confirm-message">{{ pending.message }}</p>
    <template #actions>
        <UiButton variant="ghost" :data-autofocus="pending.danger ? '' : undefined" @click="settle(false)">
            {{ pending.cancelLabel ?? 'Cancel' }}
        </UiButton>
        <UiButton
            :variant="pending.danger ? 'danger' : 'primary'"
            :data-autofocus="pending.danger ? undefined : ''"
            @click="settle(true)"
        >
            {{ pending.confirmLabel ?? 'OK' }}
        </UiButton>
    </template>
</ModalDialog>
</template>

<style scoped>
.confirm-message {
    margin: 0;
    font-size: var(--text-body-sm);
    line-height: 1.55;
    color: var(--text-secondary);
}
</style>
