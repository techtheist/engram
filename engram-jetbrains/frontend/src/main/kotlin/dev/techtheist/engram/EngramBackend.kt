package dev.techtheist.engram

import com.intellij.openapi.project.Project
import com.intellij.openapi.util.SystemInfo
import java.io.File
import java.nio.file.Files
import java.nio.file.Path

/**
 * Local `engram` binary discovery and daemon startup, backing the tool
 * window's setup card (PLAN §8 onboarding path 1: the plugin surfaces the
 * install one-liner; path 4: it can start an installed daemon itself).
 * Installation is never performed silently — the card only offers the
 * command for the user to run.
 */
object EngramBackend {
    const val INSTALL_COMMAND: String =
        "curl -fsSL https://raw.githubusercontent.com/techtheist/engram/main/install.sh | sh"

    /** PATH first, then the two conventional install locations. */
    fun findBinary(): Path? {
        val exe = if (SystemInfo.isWindows) "engram.exe" else "engram"
        val home = System.getProperty("user.home")
        val candidates = buildList {
            System.getenv("PATH")
                ?.split(File.pathSeparator)
                ?.filter { it.isNotBlank() }
                ?.forEach { add(Path.of(it, exe)) }
            add(Path.of(home, ".local", "bin", exe))
            add(Path.of(home, ".cargo", "bin", exe))
        }
        return candidates.firstOrNull { runCatching { Files.isExecutable(it) }.getOrDefault(false) }
    }

    /**
     * Launch `engram serve` detached in the project root (the daemon resolves
     * `--db` against its cwd — starting anywhere else creates a fresh empty
     * graph). Output goes to `.engram/serve.log`, same as deploy-pane.sh.
     */
    fun startDaemon(project: Project?): Boolean {
        val bin = findBinary() ?: return false
        val base = project?.basePath ?: return false
        return runCatching {
            val log = Path.of(base, ".engram", "serve.log")
            Files.createDirectories(log.parent)
            ProcessBuilder(bin.toString(), "serve")
                .directory(File(base))
                .redirectErrorStream(true)
                .redirectOutput(log.toFile())
                .start()
        }.isSuccess
    }
}
