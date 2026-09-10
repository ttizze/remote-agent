import org.gradle.api.tasks.Exec

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

val generateAgentBindings by
    tasks.registering(Exec::class) {
        workingDir(rootProject.projectDir)
        commandLine("scripts/build-agent-bindings.sh")
        inputs.file(rootProject.file("scripts/build-agent-bindings.sh"))
        inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
        inputs.dir(rootProject.file("crates/agent-ffi"))
        inputs.dir(rootProject.file("crates/agent-core"))
        outputs.dir(rootProject.file("target/agent-bindings"))
    }
val buildAgentAndroid by
    tasks.registering(Exec::class) {
        workingDir(rootProject.projectDir)
        commandLine(
            "cargo",
            "ndk",
            "--target",
            "arm64-v8a",
            "--target",
            "x86_64",
            "--output-dir",
            layout.buildDirectory.dir("generated/jniLibs").get().asFile.absolutePath,
            "build",
            "--package",
            "agent-ffi",
            "--release",
        )
        inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
        inputs.dir(rootProject.file("crates/agent-ffi"))
        inputs.dir(rootProject.file("crates/agent-core"))
        outputs.dir(layout.buildDirectory.dir("generated/jniLibs"))
    }

tasks
    .matching { it.name.startsWith("compile") && it.name.endsWith("Kotlin") }
    .configureEach { dependsOn(generateAgentBindings) }

tasks.matching { it.name.matches(Regex("merge.*JniLibFolders")) }.configureEach { dependsOn(buildAgentAndroid) }

android {
    namespace = "dev.remoteagent.mobile"
    compileSdk = 36
    defaultConfig {
        applicationId = "dev.remoteagent.mobile"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures { compose = true }
    sourceSets.getByName("main") {
        kotlin.srcDir(rootProject.file("target/agent-bindings/dev"))
        jniLibs.srcDir(layout.buildDirectory.dir("generated/jniLibs").get().asFile)
    }
}

dependencies {
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("org.jetbrains.compose.foundation:foundation:1.11.1")
    implementation("org.jetbrains.compose.material3:material3:1.11.0-alpha07")
    implementation("org.jetbrains.compose.ui:ui:1.11.1")
    implementation("androidx.camera:camera-camera2:1.6.1")
    implementation("androidx.camera:camera-lifecycle:1.6.1")
    implementation("androidx.camera:camera-view:1.6.1")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
}
