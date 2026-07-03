import * as vscode from 'vscode'

interface McpConfig {
    mcpServers?: Record<string, unknown>
    [key: string]: unknown
}

/**
 * Merge an `engram` server into the workspace `.mcp.json` (the file Claude Code
 * reads), without clobbering other servers. Uses a relative db path so it
 * resolves against the repo the daemon also uses.
 */
export async function configureMcp(): Promise<void> {
    const folder = vscode.workspace.workspaceFolders?.[0]
    if (!folder) {
        void vscode.window.showErrorMessage('Engram: open a folder before configuring MCP.')
        return
    }

    const uri = vscode.Uri.joinPath(folder.uri, '.mcp.json')
    let config: McpConfig = {}
    try {
        const bytes = await vscode.workspace.fs.readFile(uri)
        config = JSON.parse(Buffer.from(bytes).toString('utf8')) as McpConfig
    } catch {
        // No file yet (or unparseable) — start fresh.
    }
    if (typeof config.mcpServers !== 'object' || config.mcpServers === null) {
        config.mcpServers = {}
    }

    ;(config.mcpServers as Record<string, unknown>).engram = {
        command: 'engram',
        args: ['mcp', '--db', '.engram/graph.db'],
    }

    const body = JSON.stringify(config, null, 2) + '\n'
    await vscode.workspace.fs.writeFile(uri, Buffer.from(body, 'utf8'))

    const open = 'Open .mcp.json'
    const choice = await vscode.window.showInformationMessage(
        'Engram MCP added to .mcp.json. Restart Claude Code to pick it up. (Requires the `engram` binary on your PATH.)',
        open,
    )
    if (choice === open) {
        await vscode.window.showTextDocument(await vscode.workspace.openTextDocument(uri))
    }
}
