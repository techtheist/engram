import { createApp } from 'vue'
import { createPinia } from 'pinia'

// Vue Flow core + addon styles, then our token/Tailwind layer last so our
// theme variables win over the library defaults.
import '@vue-flow/core/dist/style.css'
import '@vue-flow/core/dist/theme-default.css'
import '@vue-flow/controls/dist/style.css'
import '@vue-flow/minimap/dist/style.css'
import './assets/app.css'

import App from './App.vue'

createApp(App).use(createPinia()).mount('#app')
