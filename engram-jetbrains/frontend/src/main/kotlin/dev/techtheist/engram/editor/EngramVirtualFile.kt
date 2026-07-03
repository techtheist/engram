package dev.techtheist.engram.editor

import com.intellij.testFramework.LightVirtualFile

/**
 * Marker file that opens the Engram pane as a center editor tab. All instances
 * are equal, so reopening focuses the existing tab instead of stacking copies.
 */
internal class EngramVirtualFile : LightVirtualFile("Engram") {
    override fun equals(other: Any?): Boolean = other is EngramVirtualFile
    override fun hashCode(): Int = javaClass.hashCode()
}
