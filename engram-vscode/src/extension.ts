import * as vscode from 'vscode'
import { configureMcp } from './mcp'
import { buildPaneHtml, paneOptions } from './pane'
import { createStatusBar, installBackend, offerSetupIfNeeded, startDaemon } from './setup'

const VIEW_ID = 'engram.pane'
let editorPanel: vscode.WebviewPanel | undefined

export function activate(context: vscode.ExtensionContext): void {
    const provider = new EngramViewProvider(context.extensionUri)

    context.subscriptions.push(
        vscode.window.registerWebviewViewProvider(VIEW_ID, provider, {
            webviewOptions: { retainContextWhenHidden: true },
        }),
        vscode.commands.registerCommand('engram.openInEditor', () => openInEditor(context)),
        vscode.commands.registerCommand('engram.configureMcp', configureMcp),
        vscode.commands.registerCommand('engram.installBackend', installBackend),
        vscode.commands.registerCommand('engram.startDaemon', startDaemon),
        vscode.commands.registerCommand('engram.reload', () => {
            provider.reload()
            if (editorPanel) editorPanel.webview.html = buildPaneHtml(editorPanel.webview, context.extensionUri)
        }),
    )

    createStatusBar(context)
    void offerSetupIfNeeded(context)
}

export function deactivate(): void {
    editorPanel?.dispose()
}

class EngramViewProvider implements vscode.WebviewViewProvider {
    private view?: vscode.WebviewView

    constructor(private readonly extensionUri: vscode.Uri) {}

    resolveWebviewView(view: vscode.WebviewView): void {
        this.view = view
        view.webview.options = paneOptions(this.extensionUri)
        view.webview.html = buildPaneHtml(view.webview, this.extensionUri)
    }

    reload(): void {
        if (this.view) this.view.webview.html = buildPaneHtml(this.view.webview, this.extensionUri)
    }
}

function openInEditor(context: vscode.ExtensionContext): void {
    if (editorPanel) {
        editorPanel.reveal()
        return
    }
    editorPanel = vscode.window.createWebviewPanel(
        'engram.editor',
        'Engram',
        vscode.ViewColumn.Active,
        paneOptions(context.extensionUri),
    )
    editorPanel.webview.html = buildPaneHtml(editorPanel.webview, context.extensionUri)
    editorPanel.onDidDispose(() => (editorPanel = undefined), null, context.subscriptions)
}
