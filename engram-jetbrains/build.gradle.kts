import org.jetbrains.intellij.platform.gradle.IntelliJPlatformType
import org.jetbrains.intellij.platform.gradle.TestFrameworkType
import org.jetbrains.intellij.platform.gradle.tasks.aware.SplitModeAware

group = providers.gradleProperty("pluginGroup").get()
version = providers.gradleProperty("pluginVersion").get()

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
    splitMode = true
    pluginInstallationTarget = SplitModeAware.PluginInstallationTarget.BOTH

    pluginConfiguration {
        ideaVersion {
            // 2026.1 == build 261; leave untilBuild open so alpha installs on later builds.
            sinceBuild = "261"
            untilBuild = provider { null }
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
            create(IntelliJPlatformType.IntellijIdeaUltimate, intellijPlatformVersion)
            // JCEF classes moved plugins in 262 — also verify against a newer IDE
            // when one is installed locally (CI just skips this).
            file("/Applications/IntelliJ IDEA 2026.2 EAP.app").takeIf { it.exists() }?.let { local(it) }
        }
    }
}

// Ship the README inside the distribution zip so an install-from-disk user
// gets the requirements/setup story offline (the Marketplace shows plugin.xml).
tasks.named<Zip>("buildPlugin") {
    from(layout.projectDirectory.file("README.md"))
}
