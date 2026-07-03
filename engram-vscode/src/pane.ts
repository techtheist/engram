import { readFileSync } from 'fs'
import * as vscode from 'vscode'
import { daemonUrl } from './daemon'

function nonce(): string {
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
    let out = ''
    for (let i = 0; i < 32; i++) out += chars[Math.floor(Math.random() * chars.length)]
    return out
}

/** Webview options that permit scripts and loading the bundled pane assets. */
export function paneOptions(
    extensionUri: vscode.Uri,
): vscode.WebviewOptions & vscode.WebviewPanelOptions {
    return {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [vscode.Uri.joinPath(extensionUri, 'media')],
    }
}

/**
 * Build the pane HTML from the bundled SPA's index.html. The SPA is loaded from
 * the extension (not the daemon), so we:
 *   - point a <base> at the webview-served pane folder (assets are relative),
 *   - inject the daemon URL as `window.__ENGRAM_API__` for the SPA's API calls,
 *   - set a CSP that allows the bundled assets + the daemon connection + fonts.
 * The daemon-down state is handled by the SPA itself (its own retry overlay).
 */
export function buildPaneHtml(webview: vscode.Webview, extensionUri: vscode.Uri): string {
    const paneRoot = vscode.Uri.joinPath(extensionUri, 'media', 'pane')
    const baseHref = webview.asWebviewUri(paneRoot).toString().replace(/\/?$/, '/')
    const api = daemonUrl()
    const apiAlt = api.replace('127.0.0.1', 'localhost')
    const n = nonce()

    const csp = [
        `default-src 'none'`,
        `img-src ${webview.cspSource} data:`,
        `font-src ${webview.cspSource} https://fonts.gstatic.com`,
        `style-src ${webview.cspSource} 'unsafe-inline' https://fonts.googleapis.com`,
        `script-src ${webview.cspSource} 'nonce-${n}'`,
        `connect-src ${api} ${apiAlt}`,
    ].join('; ')

    const injected = `
    <base href="${baseHref}">
    <meta http-equiv="Content-Security-Policy" content="${csp}">
    <script nonce="${n}">window.__ENGRAM_API__ = ${JSON.stringify(api)};</script>`

    let html: string
    try {
        html = readFileSync(vscode.Uri.joinPath(paneRoot, 'index.html').fsPath, 'utf8')
    } catch {
        return fallbackHtml(api)
    }
    return html.replace(/<head>/i, `<head>${injected}`)
}

function fallbackHtml(api: string): string {
    return `<!DOCTYPE html><html><body style="font-family:sans-serif;padding:2rem">
    <h3>Engram pane failed to load</h3>
    <p>The bundled pane assets are missing from this build.</p>
    <p>Daemon: <code>${api}</code></p>
    </body></html>`
}
