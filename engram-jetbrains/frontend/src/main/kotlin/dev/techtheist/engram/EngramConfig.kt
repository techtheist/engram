package dev.techtheist.engram

import com.intellij.openapi.project.Project
import java.nio.file.Files
import java.nio.file.Path

/**
 * Where the pane lives. The local `engram-alpha serve` daemon hosts both the JSON API
 * and the built Vue pane. It binds 8787 by default but walks to the next free
 * port when that's taken (one daemon per repo makes collisions routine) and
 * records the result in `.engram/daemon.json` — so the URL is resolved from
 * that file per project, falling back to the default. Resolution is re-run on
 * every health poll, so a daemon (re)started on a different port is picked up
 * without any user action.
 */
object EngramConfig {
    const val DEFAULT_PANE_URL: String = "http://127.0.0.1:8787"

    private val PORT_REGEX = Regex("\"port\"\\s*:\\s*(\\d+)")

    fun paneUrl(project: Project?): String {
        val base = project?.basePath ?: return DEFAULT_PANE_URL
        val daemonFile = Path.of(base, ".engram", "daemon.json")
        val port = try {
            PORT_REGEX.find(Files.readString(daemonFile))?.groupValues?.get(1)?.toIntOrNull()
        } catch (_: Exception) {
            null
        }
        return if (port != null) "http://127.0.0.1:$port" else DEFAULT_PANE_URL
    }

    fun healthUrl(project: Project?): String = paneUrl(project) + "/health"
}
