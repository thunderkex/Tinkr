plugins {
    id("org.jetbrains.kotlin.jvm") version "2.4.21"
    id("org.jetbrains.intellij.platform") version "2.2.1"
}

group = "dev.tinkr"
version = providers.gradleProperty("pluginVersion").orElse("0.1.0").get()

kotlin {
    jvmToolchain(21)
}

repositories {
    mavenCentral()
    intellijPlatform { defaultRepositories() }
}

dependencies {
    intellijPlatform { intellijIdeaCommunity("2024.3") }
}

intellijPlatform {
    pluginConfiguration {
        name = "Tinkr"
        version = project.version.toString()
        ideaVersion {
            sinceBuild = "243"
        }
    }
    publishing {
        token = providers.environmentVariable("PUBLISH_TOKEN")
    }
}
