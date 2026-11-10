import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, extname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Plugin } from 'vite'
import { defineConfig } from 'vitepress'

// The site is built straight from the repository: docs/ stays the one source
// (it still reads fine on GitHub), CHANGELOG.md and SECURITY.md become pages,
// and website/ adds only the landing page and the theme.
const WEBSITE = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const REPO = resolve(WEBSITE, '..')
const GITHUB = 'https://github.com/techtheist/engram'

// Published on GitHub Pages under /engram/; the demo owns /engram/demo/.
const BASE = '/engram/'

// Every top-level entry except these stays out of the page glob, so a new
// markdown file elsewhere in the repo never turns into a page by accident.
const KEEP = new Set(['docs', 'website', 'CHANGELOG.md', 'SECURITY.md'])
const srcExclude = readdirSync(REPO)
  .filter((name) => !KEEP.has(name))
  .map((name) => (statSync(join(REPO, name)).isDirectory() ? `${name}/**` : name))
  .concat(['website/node_modules/**', 'website/README.md'])

// Source file → page path.
const rewrites: Record<string, string> = {
  'website/index.md': 'index.md',
  'docs/README.md': 'docs/index.md',
  'CHANGELOG.md': 'changelog.md',
  'SECURITY.md': 'security.md',
}

/** A repo-relative source path → its URL on the site, or null if it isn't a page. */
function pageUrl(source: string): string | null {
  const page = rewrites[source] ?? (source.startsWith('docs/') ? source : null)
  if (!page) return null
  return '/' + page.replace(/(^|\/)index\.md$/, '$1').replace(/\.md$/, '')
}

/**
 * Relative links in the sources point at files as they sit in the repo
 * (`../SECURITY.md`, `../crates/…/redact.rs`). Resolve each against its own
 * file: a page becomes its site URL, anything else its GitHub view.
 */
function resolveRepoLink(href: string, file: string): string | null {
  if (/^([a-z]+:|#|\/)/i.test(href)) return null
  const [path, hash = ''] = href.split('#')
  const target = resolve(dirname(file), decodeURI(path))
  const source = relative(REPO, target)
  if (source.startsWith('..')) return null
  const anchor = hash ? `#${hash}` : ''
  const page = pageUrl(source)
  if (page) return page + anchor
  if (source.startsWith('.screenshots/')) return null
  const kind = existsSync(target) && statSync(target).isDirectory() ? 'tree' : 'blob'
  return `${GITHUB}/${kind}/main/${source}${anchor}`
}

// `vitepress dev` serves the built demo at /engram/demo/ so the Demo link
// works locally (`bun run demo` builds it into frontend/dist-demo/).
function serveDemo(): Plugin {
  const root = join(REPO, 'frontend/dist-demo')
  const types: Record<string, string> = {
    '.html': 'text/html',
    '.js': 'text/javascript',
    '.css': 'text/css',
    '.svg': 'image/svg+xml',
    '.png': 'image/png',
    '.ico': 'image/x-icon',
    '.json': 'application/json',
    '.woff2': 'font/woff2',
  }
  return {
    name: 'engram-serve-demo',
    configureServer(server) {
      server.middlewares.use(`${BASE}demo`, (req, res, next) => {
        const path = decodeURI((req.url ?? '/').split('?')[0])
        if (path === '' || !path.startsWith('/')) {
          res.statusCode = 301
          res.setHeader('Location', `${BASE}demo/`)
          return res.end()
        }
        let file = join(root, path)
        if (!file.startsWith(root)) return next()
        if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html')
        if (!existsSync(file)) {
          res.statusCode = 404
          return res.end('The demo is not built — run `bun run demo` in website/.')
        }
        res.setHeader('Content-Type', types[extname(file)] ?? 'application/octet-stream')
        res.end(readFileSync(file))
      })
    },
  }
}

export default defineConfig({
  title: 'Engram Alpha',
  description: 'Graph-based long-term project memory for AI coding assistants.',
  base: BASE,
  srcDir: '..',
  srcExclude,
  rewrites,
  cleanUrls: true,
  // The three Engram skins replace VitePress's light/dark switch (theme/).
  appearance: false,
  lastUpdated: false,

  head: [
    ['link', { rel: 'icon', type: 'image/svg+xml', href: `${BASE}favicon.svg` }],
    ['link', { rel: 'alternate icon', href: `${BASE}favicon.ico` }],
    ['link', { rel: 'preconnect', href: 'https://fonts.googleapis.com' }],
    ['link', { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: '' }],
    [
      'link',
      {
        rel: 'stylesheet',
        href: 'https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500&display=swap',
      },
    ],
    // Apply the remembered skin before first paint. The key is the pane's,
    // so a skin picked here carries into the demo (same origin) and back.
    [
      'script',
      {},
      `(function(){var t='engram-purple';try{var s=localStorage.getItem('engram.theme');if(s==='engram-purple'||s==='vscode-light'||s==='vscode-dark')t=s}catch(e){}var d=document.documentElement;d.setAttribute('data-theme',t);d.classList.toggle('dark',t!=='vscode-light')})()`,
    ],
  ],

  markdown: {
    theme: { light: 'github-light', dark: 'github-dark' },
    config(md) {
      const render = md.renderer.rules.link_open
      md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
        const href = tokens[idx].attrGet('href')
        const file = env.realPath ?? env.path
        const resolved = href && file ? resolveRepoLink(href, file) : null
        if (resolved) tokens[idx].attrSet('href', resolved)
        return render ? render(tokens, idx, options, env, self) : self.renderToken(tokens, idx, options)
      }
    },
  },

  transformPageData(page) {
    // The changelog is long: its outline lists releases only.
    if (page.relativePath === 'changelog.md') page.frontmatter.outline = 2
  },

  themeConfig: {
    siteTitle: 'Engram Alpha',
    nav: [
      { text: 'Docs', link: '/docs/', activeMatch: '^/docs/' },
      { text: 'Changelog', link: '/changelog' },
      // target makes it a full page load: the demo is a separate app.
      { text: 'Demo', link: '/demo/', target: '_self' },
    ],
    socialLinks: [{ icon: 'github', link: GITHUB }],
    sidebar: [
      {
        text: 'Start',
        items: [
          { text: 'Overview', link: '/docs/' },
          { text: 'Getting started', link: '/docs/getting-started' },
        ],
      },
      {
        text: 'Concepts',
        items: [
          { text: 'The memory model', link: '/docs/memory-model' },
          { text: 'Trust & decay', link: '/docs/trust' },
          { text: 'Recall & capture', link: '/docs/recall-and-capture' },
          { text: 'Conflicts & Checkup', link: '/docs/conflicts-and-checkup' },
        ],
      },
      {
        text: 'Using Engram',
        items: [
          { text: 'The pane', link: '/docs/pane' },
          { text: 'Customization', link: '/docs/customization' },
          { text: 'Multi-project memory', link: '/docs/multi-project' },
          { text: 'Local models', link: '/docs/models' },
        ],
      },
      {
        text: 'Under the hood',
        items: [
          { text: 'Runtime architecture', link: '/docs/runtime' },
          { text: 'Storage & TepinDB', link: '/docs/storage' },
          { text: 'Troubleshooting', link: '/docs/troubleshooting' },
        ],
      },
      {
        text: 'Project',
        items: [
          { text: 'Changelog', link: '/changelog' },
          { text: 'Security', link: '/security' },
        ],
      },
    ],
    outline: { level: [2, 3], label: 'On this page' },
    search: {
      provider: 'local',
      options: {
        // The changelog stays a page but out of the index: release notes
        // restate every feature and crowded out the docs that explain them.
        _render(src, env, md) {
          const html = md.render(src, env)
          return /^changelog\.md$/i.test(env.relativePath) ? '' : html
        },
        miniSearch: {
          searchOptions: { fuzzy: 0.2, prefix: true, boost: { title: 4, text: 2, titles: 1 } },
        },
      },
    },
    editLink: {
      pattern: `${GITHUB}/edit/main/:path`,
      text: 'Edit this page on GitHub',
    },
    docFooter: { prev: 'Previous', next: 'Next' },
    footer: {
      message: `<a href="${GITHUB}/blob/main/LICENSE">MIT licensed</a>`,
    },
  },

  vite: {
    publicDir: join(REPO, 'frontend/public'),
    plugins: [serveDemo()],
    // Pages compiled from files outside website/ (docs/, CHANGELOG.md) look
    // for vue next to themselves; send them to website/node_modules.
    resolve: {
      alias: [{ find: /^vue($|\/)/, replacement: join(WEBSITE, 'node_modules/vue') + '$1' }],
    },
    server: { fs: { allow: [REPO] } },
  },
})
