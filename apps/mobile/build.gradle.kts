import groovy.json.JsonSlurper
import java.io.File
import java.util.Properties
import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.PathSensitivity

plugins {
    id("com.ncorti.ktfmt.gradle")
    id("io.gitlab.arturbosch.detekt")
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

ktfmt {
    kotlinLangStyle()
    maxWidth.set(120)
}

detekt {
    toolVersion = "1.23.8"
    buildUponDefaultConfig = true
    config.setFrom(rootProject.file("detekt.yml"))
    source.setFrom("src/main/kotlin")
    basePath = rootProject.projectDir.absolutePath
}

val buildRevision =
    providers
        .exec {
            workingDir(rootProject.projectDir)
            commandLine("git", "describe", "--always", "--dirty", "--abbrev=40", "--exclude", "*")
        }
        .standardOutput
        .asText

val nativeAbis = listOf("arm64-v8a", "x86_64")
val rustBuildInputs =
    rootProject.files(
        "Cargo.toml",
        "Cargo.lock",
        "flake.nix",
        "flake.lock",
        ".cargo/config.toml",
        "scripts/rust-build-inputs.ts",
        "apps/mobile/build.gradle.kts",
        "build.gradle.kts",
        "settings.gradle.kts",
        "gradle.properties",
    )
val rustMetadata =
    providers
        .exec {
            workingDir(rootProject.projectDir)
            commandLine("node", "scripts/rust-build-inputs.ts", "agent-core", "agent-bindgen", "agent-ffi")
        }
        .standardOutput
        .asText
        .map { JsonSlurper().parseText(it) as Map<*, *> }
val rustSources = rustMetadata.map { metadata ->
    (metadata["sources"] as Map<*, *>).entries.associate { (name, paths) ->
        (name as String) to (paths as List<*>).map { File(it as String) }
    }
}
val ndkHome = providers.environmentVariable("ANDROID_NDK_HOME").get()

val generateAgentBindings =
    tasks.register<Exec>("generateAgentBindings") {
        workingDir(rootProject.projectDir)
        commandLine("scripts/build-agent-bindings.sh")
        inputs.file(rootProject.file("scripts/build-agent-bindings.sh")).withPathSensitivity(PathSensitivity.RELATIVE)
        inputs
            .files(rustSources.map { it.getValue("agent-core") + it.getValue("agent-bindgen") })
            .withPropertyName("rustSources")
            .withPathSensitivity(PathSensitivity.RELATIVE)
        outputs.dir(rootProject.file("target/agent-bindings"))
    }
val buildAgentAndroid =
    tasks.register<Exec>("buildAgentAndroid") {
        workingDir(rootProject.projectDir)
        commandLine(
            listOf(
                "node",
                "scripts/build-agent-android.ts",
                rootProject.relativePath(layout.buildDirectory.dir("generated/jniLibs").get().asFile),
            ) + nativeAbis
        )
        inputs.file(rootProject.file("scripts/build-agent-android.ts")).withPathSensitivity(PathSensitivity.RELATIVE)
        inputs.property("nativeAbis", nativeAbis)
        inputs.file("$ndkHome/source.properties").withPathSensitivity(PathSensitivity.NONE)
        inputs
            .files(rustSources.map { it.getValue("agent-ffi") })
            .withPropertyName("rustSources")
            .withPathSensitivity(PathSensitivity.RELATIVE)
        outputs.dir(layout.buildDirectory.dir("generated/jniLibs"))
    }

// When requested together, finish static checks before starting Rust builds.
listOf(generateAgentBindings, buildAgentAndroid).forEach { task ->
    task.configure {
        mustRunAfter("ktfmtCheck", "detekt")
        inputs.files(rustBuildInputs).withPropertyName("rustBuild").withPathSensitivity(PathSensitivity.RELATIVE)
        inputs.property("compiler", rustMetadata.map { it["compiler"]!! })
        outputs.cacheIf { true }
    }
}

tasks
    .matching { it.name.startsWith("compile") && it.name.endsWith("Kotlin") }
    .configureEach { dependsOn(generateAgentBindings) }

tasks.matching { it.name.matches(Regex("merge.*JniLibFolders")) }.configureEach { dependsOn(buildAgentAndroid) }

android {
    // AGP no longer reads ANDROID_NDK_HOME; use the same pinned NDK as cargo-ndk.
    ndkPath = ndkHome
    ndkVersion =
        Properties()
            .apply { file("$ndkHome/source.properties").inputStream().use { load(it) } }
            .getProperty("Pkg.Revision")
    namespace = "dev.remoteagent.mobile"
    compileSdk = 37
    defaultConfig {
        applicationId = "dev.remoteagent.mobile"
        minSdk = 26
        targetSdk = 37
        versionCode = System.getenv("AGENT_ANDROID_VERSION_CODE")?.toIntOrNull() ?: 1
        versionName = System.getenv("AGENT_ANDROID_VERSION_CODE")?.let { "0.1.0.$it" } ?: "0.1.0"
        buildConfigField("String", "BUILD_REVISION", "\"${buildRevision.get().trim()}\"")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk { abiFilters += nativeAbis }
    }
    signingConfigs {
        System.getenv("AGENT_ANDROID_KEYSTORE")?.let { path ->
            create("distribution") {
                storeFile = file(path)
                storeType = "pkcs12"
                storePassword = providers.environmentVariable("AGENT_ANDROID_KEYSTORE_PASSWORD").get()
                keyAlias = providers.environmentVariable("AGENT_ANDROID_KEY_ALIAS").get()
                keyPassword = storePassword
            }
        }
    }
    buildTypes { getByName("release") { signingConfig = signingConfigs.findByName("distribution") } }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    sourceSets.getByName("androidTest").assets.directories +=
        rootProject.file("crates/agent-core/tests/fixtures/markdown").path
    sourceSets.getByName("main") {
        assets.directories += project.file("file-icons").path
        kotlin.directories += rootProject.file("target/agent-bindings/dev").path
        jniLibs.directories += layout.buildDirectory.dir("generated/jniLibs").get().asFile.path
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    implementation(project(":terminal-native"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4:1.12.1")
    debugImplementation("androidx.compose.ui:ui-test-manifest:1.12.1")
    androidTestImplementation("androidx.test.uiautomator:uiautomator:2.4.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    // Compose UI tests pull an older Espresso that calls InputManager.getInstance,
    // which Android 17 removed; pin the version that supports it.
    androidTestImplementation("androidx.test.espresso:espresso-core:3.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.12.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("com.revenuecat.purchases:placeholder:1.0.4")
    implementation("org.jetbrains.compose.foundation:foundation:1.12.2")
    // Compose Multiplatform publishes material3 for its 1.12 line only as alphas.
    implementation("org.jetbrains.compose.material3:material3:1.12.0-alpha03")
    implementation("org.jetbrains.compose.ui:ui:1.12.2")
    implementation("androidx.camera:camera-camera2:1.6.2")
    implementation("androidx.camera:camera-lifecycle:1.6.2")
    implementation("androidx.camera:camera-view:1.6.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
}
