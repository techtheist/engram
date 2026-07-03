import pluginVue from 'eslint-plugin-vue'
import { defineConfigWithVueTs, vueTsConfigs } from '@vue/eslint-config-typescript'

export default defineConfigWithVueTs(
  { files: ['**/*.{ts,mts,tsx,vue}'] },
  { ignores: ['dist/**', 'node_modules/**'] },
  pluginVue.configs['flat/recommended'],
  vueTsConfigs.recommended,
  {
    rules: {
      // Codebase conventions: 4-space templates with the root element at
      // column 0, and self-closed void elements (<input />).
      'vue/html-indent': ['error', 4, { baseIndent: 0 }],
      'vue/html-self-closing': ['warn', { html: { void: 'always', normal: 'always', component: 'always' } }],
      'vue/max-attributes-per-line': 'off',
      'vue/singleline-html-element-content-newline': 'off',
    },
  },
)
