import DefaultTheme from 'vitepress/theme'
import type { Theme } from 'vitepress'
import { h, onMounted } from 'vue'
import ThemeSwitch from './ThemeSwitch.vue'
import Landing from './Landing.vue'
import BrandIcon from './BrandIcon.vue'
import { syncSkin } from './theme'
import './style.css'

export default {
  extends: DefaultTheme,
  Layout: () =>
    h(DefaultTheme.Layout, null, {
      'nav-bar-title-before': () => h(BrandIcon, { size: 24, class: 'nav-logo' }),
      'nav-bar-content-after': () => h(ThemeSwitch),
      'nav-screen-content-after': () => h(ThemeSwitch, { wide: true }),
    }),
  setup() {
    onMounted(syncSkin)
  },
  enhanceApp({ app }) {
    app.component('Landing', Landing)
  },
} satisfies Theme
