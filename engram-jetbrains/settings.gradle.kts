@file:Suppress("UnstableApiUsage")

import org.jetbrains.intellij.platform.gradle.extensions.intellijPlatform

pluginManagement {
    repositories {
        mavenCentral()
        gradlePluginPortal()
        maven("https://packages.jetbrains.team/maven/p/ij/intellij-dependencies/")
    }
    plugins {
        id("org.jetbrains.kotlin.jvm") version "2.3.20"
    }
}

plugins {
    // Auto-provisions the JDK 21 toolchain the IntelliJ Platform build requires.
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
    id("org.jetbrains.intellij.platform.settings") version "2.16.0"
}

rootProject.name = "engram"

dependencyResolutionManagement {
    repositories {
        mavenCentral()
        intellijPlatform {
            defaultRepositories()
        }
    }
}

include("shared")
include("frontend")
include("backend")
