import { readFileSync } from 'fs'
import { join } from 'path'
import * as vscode from 'vscode'

/**
 * The `engram-alpha serve` URL. The daemon binds 8787 by default but walks to the
 * next free port when it's taken (one daemon per repo makes collisions
 * routine) and records the result in `.engram/daemon.json` — so prefer that
 * file from the workspace, then the `engram.daemonUrl` setting.
 */
export function daemonUrl(): string {
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
        try {
            const raw = readFileSync(join(folder.uri.fsPath, '.engram', 'daemon.json'), 'utf8')
            const port = (JSON.parse(raw) as { port?: unknown }).port
            if (typeof port === 'number' && Number.isInteger(port)) {
                return `http://127.0.0.1:${port}`
            }
        } catch {
            /* no daemon file in this folder — fall through */
        }
    }
    return vscode.workspace.getConfiguration('engram').get('daemonUrl', 'http://127.0.0.1:8787')
}
