dependencies {
    intellijPlatform {
        bundledModule("intellij.platform.frontend")
    }
}

// The 261 artifact must NOT declare the intellij.platform.ui.jcef module dep:
// a 2026.1 classic runtime can't resolve that name and disables the whole
// module (issue #1) — there JBCefApp is still in core, no declaration needed.
tasks.processResources {
    val platformLine = providers.gradleProperty("platformLine").getOrElse("262")
    // The filter lambda is invisible to up-to-date checks — without this input
    // a 261 build's stripped descriptor survives into the next 262 build.
    inputs.property("platformLine", platformLine)
    if (platformLine == "261") {
        filter { line: String ->
            if (line.contains("<module name=\"intellij.platform.ui.jcef\"/>")) null else line
        }
    }
}
