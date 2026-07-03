package dev.techtheist.engram.toolWindow

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBPanel
import com.intellij.ui.jcef.JBCefApp
import com.intellij.ui.jcef.JBCefBrowser
import com.intellij.util.Alarm
import com.intellij.util.ui.JBFont
import com.intellij.util.ui.JBUI
import dev.techtheist.engram.EngramBackend
import dev.techtheist.engram.EngramConfig
import java.awt.BorderLayout
import java.awt.CardLayout
import java.awt.Component
import java.awt.datatransfer.StringSelection
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Duration
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JComponent

/**
 * The Engram tool window contents. Hosts the Vue pane (served by the local
 * `engram serve` daemon) in a JCEF browser, and degrades to a clear
 * "backend not running" panel — with start/install guidance — when the daemon
 * can't be reached, re-checking in the background so it connects on its own
 * once the daemon comes up.
 */
internal class EngramPanel(
    private val project: Project?,
    parent: Disposable,
) : JBPanel<EngramPanel>(CardLayout()), Disposable {
    private val cards get() = layout as CardLayout
    private val http = HttpClient.newBuilder().connectTimeout(CONNECT_TIMEOUT).build()
    private val pollAlarm = Alarm(Alarm.ThreadToUse.POOLED_THREAD, this)
    private var browser: JBCefBrowser? = null
    private var loadedUrl: String? = null
    private var offlineComponent: JComponent? = null
    private var offlineHasBinary: Boolean? = null

    @Volatile
    private var disposed = false

    init {
        Disposer.register(parent, this)

        if (!JBCefApp.isSupported()) {
            add(unsupportedCard(), CARD_UNSUPPORTED)
            cards.show(this, CARD_UNSUPPORTED)
        } else {
            // Windowed (non-OSR) rendering. Off-screen rendering on macOS Retina
            // loses its scale factor when the tool window is hidden and shown
            // again, repainting at 1x (the "giant pixels" bug); the native
            // windowed view keeps its scale and scrolls more smoothly.
            // Built blank — we only load the pane once /health is green, so a
            // down daemon never flashes the browser's own connection-error page.
            val b = JBCefBrowser.createBuilder()
                .setOffScreenRendering(false)
                .build()
            Disposer.register(this, b)
            browser = b

            add(b.component, CARD_PANE)
            showOfflineCard(hasBinary = EngramBackend.findBinary() != null)
            scheduleHealthCheck(immediate = true)
        }
    }

    /** Poll `/health` off the EDT; flip cards (and lazily load the pane) on the result. */
    private fun scheduleHealthCheck(immediate: Boolean) {
        if (disposed) return
        pollAlarm.addRequest({ checkHealthOnce() }, if (immediate) 0 else POLL_INTERVAL_MS)
    }

    private fun checkHealthOnce() {
        // Re-resolve the URL every poll: the daemon records its actual port in
        // .engram/daemon.json, so a daemon (re)started on a fallback port is
        // discovered automatically.
        val paneUrl = EngramConfig.paneUrl(project)
        val healthy = try {
            val req = HttpRequest.newBuilder(URI.create("$paneUrl/health"))
                .timeout(REQUEST_TIMEOUT)
                .GET()
                .build()
            http.send(req, HttpResponse.BodyHandlers.discarding()).statusCode() == 200
        } catch (_: Exception) {
            false
        }
        // Binary discovery is filesystem work — do it here, off the EDT.
        val hasBinary = if (healthy) true else EngramBackend.findBinary() != null

        ApplicationManager.getApplication().invokeLater {
            if (disposed) return@invokeLater
            if (healthy) {
                if (loadedUrl != paneUrl) {
                    browser?.loadURL(paneUrl)
                    loadedUrl = paneUrl
                }
                cards.show(this, CARD_PANE)
            } else {
                loadedUrl = null
                showOfflineCard(hasBinary)
                scheduleHealthCheck(immediate = false) // keep watching; auto-connect on return
            }
        }
    }

    private fun retryNow() {
        loadedUrl = null
        pollAlarm.cancelAllRequests()
        scheduleHealthCheck(immediate = true)
    }

    /** Swap in the offline card matching the current state, rebuilding only on change. */
    private fun showOfflineCard(hasBinary: Boolean) {
        if (offlineComponent == null || offlineHasBinary != hasBinary) {
            offlineComponent?.let { remove(it) }
            val card = if (hasBinary) daemonDownCard() else notInstalledCard()
            offlineComponent = card
            offlineHasBinary = hasBinary
            add(card, CARD_OFFLINE)
        }
        cards.show(this, CARD_OFFLINE)
    }

    private fun daemonDownCard(): JComponent = centered {
        heading("Engram backend isn't running")
        body("The graph pane is served by the local Engram daemon.")
        gap()
        button("Start engram serve") {
            if (EngramBackend.startDaemon(project)) retryNow()
        }
        gap()
        body("Or start it yourself from the repo root:")
        mono("engram serve")
        gap()
        button("Retry") { retryNow() }
    }

    private fun notInstalledCard(): JComponent = centered {
        heading("Set up Engram")
        body("The graph pane needs the local Engram backend, which isn't installed yet.")
        body("Run this from your project's root — it installs the binary and")
        body("wires the repo for your AI assistant (on Windows: inside WSL2):")
        gap()
        mono(EngramBackend.INSTALL_COMMAND)
        gap()
        button("Copy install command") {
            CopyPasteManager.getInstance().setContents(StringSelection(EngramBackend.INSTALL_COMMAND))
        }
        gap()
        body("Then start the daemon (engram serve) — the pane connects on its own.")
        button("Retry") { retryNow() }
    }

    private fun unsupportedCard(): JComponent = centered {
        heading("Embedded browser unavailable")
        body("This IDE build has no JCEF support, so the Engram pane can't render here.")
        body("Open the pane in your browser instead:")
        mono(EngramConfig.paneUrl(project))
    }

    override fun dispose() {
        disposed = true
    }

    // --- tiny Swing DSL for the message cards -----------------------------

    private class CardBuilder {
        val panel = JBPanel<JBPanel<*>>().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            border = JBUI.Borders.empty(24)
        }

        private fun add(c: JComponent) {
            c.alignmentX = Component.CENTER_ALIGNMENT
            panel.add(c)
        }

        fun heading(text: String) = add(JBLabel(text).apply { font = JBFont.h3() })
        fun body(text: String) = add(JBLabel(text))
        fun mono(text: String) = add(JBLabel(text).apply {
            font = java.awt.Font(java.awt.Font.MONOSPACED, java.awt.Font.PLAIN, font.size)
        })
        fun gap() = panel.add(javax.swing.Box.createVerticalStrut(JBUI.scale(12)))
        fun button(text: String, onClick: () -> Unit) =
            add(JButton(text).apply { addActionListener { onClick() } })
    }

    private fun centered(build: CardBuilder.() -> Unit): JComponent {
        val content = CardBuilder().apply(build).panel
        return JBPanel<JBPanel<*>>(BorderLayout()).apply { add(content, BorderLayout.CENTER) }
    }

    private companion object {
        const val CARD_PANE = "pane"
        const val CARD_OFFLINE = "offline"
        const val CARD_UNSUPPORTED = "unsupported"
        const val POLL_INTERVAL_MS = 2000
        val CONNECT_TIMEOUT: Duration = Duration.ofMillis(800)
        val REQUEST_TIMEOUT: Duration = Duration.ofMillis(800)
    }
}
