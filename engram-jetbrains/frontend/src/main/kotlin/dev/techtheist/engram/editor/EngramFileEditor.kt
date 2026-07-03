package dev.techtheist.engram.editor

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorLocation
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import dev.techtheist.engram.toolWindow.EngramPanel
import java.beans.PropertyChangeListener
import javax.swing.JComponent

/** Hosts the Engram pane in the center editor area (same JCEF panel as the tool window). */
internal class EngramFileEditor(project: Project, private val file: VirtualFile) :
    UserDataHolderBase(), FileEditor {
    private val panel = EngramPanel(project, this)

    override fun getComponent(): JComponent = panel
    override fun getPreferredFocusedComponent(): JComponent = panel
    override fun getName(): String = "Engram"
    override fun getFile(): VirtualFile = file
    override fun getCurrentLocation(): FileEditorLocation? = null
    override fun setState(state: FileEditorState) {}
    override fun isModified(): Boolean = false
    override fun isValid(): Boolean = true
    override fun addPropertyChangeListener(listener: PropertyChangeListener) {}
    override fun removePropertyChangeListener(listener: PropertyChangeListener) {}
    override fun dispose() {}
}
