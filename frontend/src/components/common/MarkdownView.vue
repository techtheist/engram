<script setup lang="ts">
import { computed } from 'vue'
import MarkdownIt from 'markdown-it'
import DOMPurify from 'dompurify'

const props = defineProps<{ content: string }>()

const md = new MarkdownIt({ html: false, linkify: true, breaks: true })
const html = computed(() => DOMPurify.sanitize(md.render(props.content)))
</script>

<template>
<!-- content is sanitized with DOMPurify before injection -->
<!-- eslint-disable-next-line vue/no-v-html -->
<div class="md text-body-sm text-text-secondary" v-html="html" />
</template>

<style scoped>
.md :where(p) {
    margin: 0 0 0.6rem;
}

.md :where(p:last-child) {
    margin-bottom: 0;
}

.md :where(code) {
    font-family: var(--font-mono);
    font-size: 0.9em;
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
    background-color: var(--surface-sunken);
}

.md :where(a) {
    color: var(--interactive-primary);
    text-decoration: underline;
}

.md :where(ul, ol) {
    margin: 0 0 0.6rem;
    padding-left: 1.8rem;
}

.md :where(strong) {
    color: var(--text-primary);
    font-weight: 600;
}
</style>
