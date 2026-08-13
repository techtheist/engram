import org.jetbrains.intellij.platform.gradle.IntelliJPlatformType
import org.jetbrains.intellij.platform.gradle.TestFrameworkType
import org.jetbrains.intellij.platform.gradle.tasks.aware.SplitModeAware

group = providers.gradleProperty("pluginGroup").get()

// One descriptor cannot serve both platform lines (issue #1): on 2026.2+ JCEF
// lives in the com.intellij.modules.jcef plugin and engram.frontend MUST
// declare the intellij.platform.ui.jcef module dep to see JBCefApp — but on a
// 2026.1 CLASSIC (single-process) runtime that module name is unresolvable
// (the dist's module-descriptors.jar only feeds the split-mode loader) and the
// declaration gets the whole module disabled. So the build produces two
// artifacts, selected by -PplatformLine:
//   261 — version "X.Y.Z-261", sinceBuild 261 / untilBuild 261.*, WITHOUT the
//         module dep (JBCefApp is reachable from core there; 0.2.1 proved it);
//   262 — (default) version "X.Y.Z", sinceBuild 262 / open-ended, WITH it.
// The Marketplace serves each IDE the artifact whose range matches.
val platformLine = providers.gradleProperty("platformLine").getOrElse("262")
version = providers.gradleProperty("pluginVersion").get() +
    if (platformLine == "261") "-261" else ""

val intellijPlatformVersion = providers.gradleProperty("intellijPlatformVersion").get()

plugins {
    application
    id("org.jetbrains.intellij.platform")
    id("org.jetbrains.kotlin.jvm")
}

subprojects {
    apply(plugin = "org.jetbrains.intellij.platform.module")
    apply(plugin = "org.jetbrains.kotlin.jvm")
}

dependencies {
    intellijPlatform {
        intellijIdea(intellijPlatformVersion)

        pluginModule(implementation(project(":shared")))
        pluginModule(implementation(project(":frontend")))
        pluginModule(implementation(project(":backend")))
        testFramework(TestFrameworkType.Platform)
    }
}

intellijPlatform {
    // Split mode resolves module descriptors the classic desktop loader can't —
    // it's exactly how issue #1 slipped past runIde. `-PsplitMode=false` runs
    // the sandbox IDE the way a user's desktop IDE actually loads the plugin.
    splitMode = providers.gradleProperty("splitMode").map(String::toBoolean).getOrElse(true)
    pluginInstallationTarget = SplitModeAware.PluginInstallationTarget.BOTH

    pluginConfiguration {
        ideaVersion {
            if (platformLine == "261") {
                sinceBuild = "261"
                untilBuild = "261.*"
            } else {
                sinceBuild = "262"
                untilBuild = provider { null }
            }
        }
    }

    // Marketplace signing + publishing. All inputs come from CI secrets; absent
    // locally, `buildPlugin` still works — only `signPlugin`/`publishPlugin` need them.
    signing {
        certificateChain = providers.environmentVariable("CERTIFICATE_CHAIN")
        privateKey = providers.environmentVariable("PRIVATE_KEY")
        password = providers.environmentVariable("PRIVATE_KEY_PASSWORD")
    }

    publishing {
        token = providers.environmentVariable("PUBLISH_TOKEN")
        // Release channel from the version: "1.2.0-beta.1" -> "beta"; a plain
        // "0.2.0" has no suffix and goes to "default" — the Marketplace Stable
        // channel, the only one users see without adding a custom channel repo.
        channels = providers.gradleProperty("pluginVersion").map {
            listOf(it.substringAfter('-', "default").substringBefore('.').ifEmpty { "default" })
        }
    }

    pluginVerification {
        ides {
            // Each artifact is verified against an IDE inside its own range —
            // the verifier flags an out-of-range IDE as a failure, not a skip.
            if (platformLine == "261") {
                create(IntelliJPlatformType.IntellijIdeaUltimate, intellijPlatformVersion)
            } else {
                create(IntelliJPlatformType.IntellijIdeaUltimate, "2026.2.1")
                file("/Applications/IntelliJ IDEA 2026.2 EAP.app").takeIf { it.exists() }?.let { local(it) }
            }
        }
    }
}

// Ship the README inside the distribution zip so an install-from-disk user
// gets the requirements/setup story offline (the Marketplace shows plugin.xml).
tasks.named<Zip>("buildPlugin") {
    from(layout.projectDirectory.file("README.md"))
}
