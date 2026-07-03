import { existsSync } from 'fs'
import { delimiter, join } from 'path'
import * as vscode from 'vscode'
import { daemonUrl } from './daemon'

/**
 * Backend setup affordances (PLAN §8 onboarding path 2): when the daemon is
 * unreachable, offer to install the backend (visible terminal, user confirms
 * by pressing Enter — never a silent download) or to start an installed one.
 */

export const INSTALL_COMMAND =
    'curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh'

/** PATH first, then the two conventional install locations. */
export function findBinary(): string | undefined {
    const exe = process.platform === 'win32' ? 'engram.exe' : 'engram'
    const home = process.env.HOME ?? process.env.USERPROFILE ?? ''
    const candidates = [
        ...(process.env.PATH ?? '')
            .split(delimiter)
            .filter(Boolean)
            .map((dir) => join(dir, exe)),
        join(home, '.local', 'bin', exe),
        join(home, '.cargo', 'bin', exe),
    ]
    return candidates.find((p) => existsSync(p))
}

export async function daemonHealthy(): Promise<boolean> {
    const ctrl = new AbortController()
    const timer = setTimeout(() => ctrl.abort(), 1500)
    try {
        const res = await fetch(`${daemonUrl()}/health`, { signal: ctrl.signal })
        return res.ok
    } catch {
        return false
    } finally {
        clearTimeout(timer)
    }
}

function workspaceRoot(): string | undefined {
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath
}

/** Pre-type the install one-liner in a terminal; the user reviews and hits Enter. */
export function installBackend(): void {
    const isWin = process.platform === 'win32'
    const terminal = vscode.window.createTerminal({
        name: 'Engram setup',
        cwd: workspaceRoot(),
        // The installer is a POSIX script; on Windows it runs inside WSL2.
        ...(isWin ? { shellPath: 'wsl.exe' } : {}),
    })
    terminal.show()
    terminal.sendText(INSTALL_COMMAND, false)
}

/** Run `engram serve` in a visible terminal at the workspace root. */
export function startDaemon(): void {
    const bin = findBinary() ?? 'engram'
    const terminal = vscode.window.createTerminal({ name: 'engram serve', cwd: workspaceRoot() })
    terminal.show()
    const quoted = bin.includes(' ') ? `"${bin}"` : bin
    terminal.sendText(process.platform === 'win32' && bin.includes(' ') ? `& ${quoted} serve` : `${quoted} serve`, true)
}

/** One-shot nudge when the workspace has no reachable daemon. */
export async function offerSetupIfNeeded(context: vscode.ExtensionContext): Promise<void> {
    if (!workspaceRoot()) return
    if (context.workspaceState.get<boolean>('engram.setupDismissed')) return
    if (await daemonHealthy()) return

    if (findBinary()) {
        const pick = await vscode.window.showInformationMessage(
            'Engram: the daemon is not running in this workspace.',
            'Start engram serve',
            'Later',
        )
        if (pick === 'Start engram serve') startDaemon()
        if (pick === 'Later') await context.workspaceState.update('engram.setupDismissed', true)
    } else {
        const pick = await vscode.window.showInformationMessage(
            'Engram: the backend is not installed. Install it to light up the graph pane and Claude Code memory.',
            'Install backend',
            'Later',
        )
        if (pick === 'Install backend') installBackend()
        if (pick === 'Later') await context.workspaceState.update('engram.setupDismissed', true)
    }
}

/** Status bar dot: daemon connectivity at a glance; click for actions. */
export function createStatusBar(context: vscode.ExtensionContext): void {
    const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100)
    item.name = 'Engram'
    item.command = 'engram.status'
    context.subscriptions.push(item)

    let healthy = false
    const refresh = async (): Promise<void> => {
        healthy = await daemonHealthy()
        item.text = healthy ? '$(pass-filled) Engram' : '$(circle-slash) Engram'
        item.tooltip = healthy
            ? `Engram daemon connected (${daemonUrl()})`
            : 'Engram daemon unreachable — click for setup options'
        item.show()
    }
    void refresh()
    const timer = setInterval(() => void refresh(), 10_000)
    context.subscriptions.push({ dispose: () => clearInterval(timer) })

    context.subscriptions.push(
        vscode.commands.registerCommand('engram.status', async () => {
            if (healthy) {
                await vscode.commands.executeCommand('engram.openInEditor')
                return
            }
            const actions = findBinary()
                ? ['Start engram serve', 'Configure MCP']
                : ['Install backend', 'Configure MCP']
            const pick = await vscode.window.showQuickPick(actions, {
                placeHolder: 'Engram daemon is unreachable',
            })
            if (pick === 'Start engram serve') startDaemon()
            if (pick === 'Install backend') installBackend()
            if (pick === 'Configure MCP') await vscode.commands.executeCommand('engram.configureMcp')
        }),
    )
}
