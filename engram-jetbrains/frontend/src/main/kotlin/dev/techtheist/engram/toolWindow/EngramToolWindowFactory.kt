package dev.techtheist.engram.toolWindow

import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.content.ContentFactory

/** Registers the Engram tool window; its content is the JCEF-hosted graph pane. */
internal class EngramToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun shouldBeAvailable(project: Project) = true

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = EngramPanel(project, toolWindow.disposable)
        val content = ContentFactory.getInstance().createContent(panel, null, false)
        toolWindow.contentManager.addContent(content)
        toolWindow.setAdditionalGearActions(DefaultActionGroup(RefreshPaneAction(panel)))
    }

    /** Gear-menu action: recreate the embedded browser and reconnect to the daemon. */
    private class RefreshPaneAction(private val panel: EngramPanel) : AnAction(
        "Refresh Pane",
        "Restart the embedded browser and reconnect to the Engram daemon",
        AllIcons.Actions.Refresh,
    ), DumbAware {
        override fun getActionUpdateThread() = ActionUpdateThread.EDT
        override fun actionPerformed(e: AnActionEvent) = panel.restartBrowser()
    }
}
