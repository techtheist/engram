import js from '@eslint/js'
import tseslint from 'typescript-eslint'

export default tseslint.config(
  { ignores: ['dist/**', 'media/**', 'node_modules/**', 'esbuild.js'] },
  js.configs.recommended,
  tseslint.configs.recommended,
)
