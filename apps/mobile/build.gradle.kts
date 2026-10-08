import com.ncorti.ktfmt.gradle.tasks.KtfmtCheckTask
import com.ncorti.ktfmt.gradle.tasks.KtfmtFormatTask
import org.gradle.api.tasks.Exec

plugins {
    id("com.ncorti.ktfmt.gradle")
    id("io.gitlab.arturbosch.detekt")
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

fun publicFirebaseOption(property: String, environment: String): String =
    providers.gradleProperty(property).orElse(providers.environmentVariable(environment)).orElse("").get()

fun buildConfigString(value: String): String {
    require(value.none { Character.isISOControl(it.code) }) {
        "Firebase public options cannot contain control characters"
    }
    return "\"${value.replace("\\", "\\\\").replace("\"", "\\\"")}\""
}

val firebaseApiKey = publicFirebaseOption("firebaseApiKey", "REMOTE_AGENT_FIREBASE_API_KEY")
val firebaseApplicationId = publicFirebaseOption("firebaseApplicationId", "REMOTE_AGENT_FIREBASE_APPLICATION_ID")
val firebaseProjectId = publicFirebaseOption("firebaseProjectId", "REMOTE_AGENT_FIREBASE_PROJECT_ID")
val firebaseSenderId = publicFirebaseOption("firebaseSenderId", "REMOTE_AGENT_FIREBASE_SENDER_ID")
val firebaseStorageBucket = publicFirebaseOption("firebaseStorageBucket", "REMOTE_AGENT_FIREBASE_STORAGE_BUCKET")

ktfmt {
    kotlinLangStyle()
    maxWidth.set(120)
}

// AGP's built-in Kotlin source set is not discovered by ktfmt-gradle.
val ktfmtFormatNative by
    tasks.registering(KtfmtFormatTask::class) {
        source = fileTree("src")
        include("**/*.kt")
    }
val ktfmtCheckNative by
    tasks.registering(KtfmtCheckTask::class) {
        source = fileTree("src")
        include("**/*.kt")
    }

tasks.named("ktfmtFormat") { dependsOn(ktfmtFormatNative) }

tasks.named("ktfmtCheck") { dependsOn(ktfmtCheckNative) }

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
        inputs.property("buildRevision", providers.environmentVariable("BEX_BUILD_REVISION").orElse("development"))
        inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
        inputs.dir(rootProject.file("crates/agent-ffi"))
        inputs.dir(rootProject.file("crates/agent-core"))
        inputs.dir(rootProject.file("crates/agent-protocol"))
        inputs.dir(rootProject.file("crates/agent-transport"))
        inputs.dir(rootProject.file("crates/agent-domain"))
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
        inputs.property("buildRevision", providers.environmentVariable("BEX_BUILD_REVISION").orElse("development"))
        inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
        inputs.dir(rootProject.file("crates/agent-ffi"))
        inputs.dir(rootProject.file("crates/agent-core"))
        inputs.dir(rootProject.file("crates/agent-protocol"))
        inputs.dir(rootProject.file("crates/agent-transport"))
        inputs.dir(rootProject.file("crates/agent-domain"))
        outputs.dir(layout.buildDirectory.dir("generated/jniLibs"))
    }

tasks
    .matching { it.name.startsWith("compile") && it.name.endsWith("Kotlin") }
    .configureEach { dependsOn(generateAgentBindings) }

tasks.matching { it.name.matches(Regex("merge.*JniLibFolders")) }.configureEach { dependsOn(buildAgentAndroid) }

val releaseSigningValues =
    listOf(
        providers.environmentVariable("ANDROID_RELEASE_KEYSTORE").orNull,
        providers.environmentVariable("ANDROID_RELEASE_KEY_ALIAS").orNull,
        providers.environmentVariable("ANDROID_RELEASE_KEYSTORE_PASSWORD").orNull,
        providers.environmentVariable("ANDROID_RELEASE_KEY_PASSWORD").orNull,
    )
val releaseSigningConfigured = releaseSigningValues.all { !it.isNullOrEmpty() }
val releaseVersion = providers.gradleProperty("releaseVersion").orNull
val releaseCode = providers.gradleProperty("releaseCode").orNull?.toIntOrNull()
val releaseChannel = providers.gradleProperty("releaseChannel").orNull ?: "stable"

require(releaseChannel in setOf("nightly", "preview", "stable")) {
    "releaseChannel must be nightly, preview, or stable"
}

android {
    namespace = "dev.remoteagent.mobile"
    compileSdk = 37
    defaultConfig {
        applicationId = "dev.remoteagent.mobile"
        minSdk = 37
        targetSdk = 37
        versionCode = releaseCode ?: 1
        versionName = releaseVersion ?: "0.1.0"
        buildConfigField("String", "RELEASE_CHANNEL", "\"$releaseChannel\"")
        buildConfigField("String", "FIREBASE_API_KEY", buildConfigString(firebaseApiKey))
        buildConfigField("String", "FIREBASE_APPLICATION_ID", buildConfigString(firebaseApplicationId))
        buildConfigField("String", "FIREBASE_PROJECT_ID", buildConfigString(firebaseProjectId))
        buildConfigField("String", "FIREBASE_SENDER_ID", buildConfigString(firebaseSenderId))
        buildConfigField("String", "FIREBASE_STORAGE_BUCKET", buildConfigString(firebaseStorageBucket))
    }
    if (releaseSigningConfigured) {
        signingConfigs {
            create("release") {
                storeFile = file(releaseSigningValues[0]!!)
                keyAlias = releaseSigningValues[1]
                storePassword = releaseSigningValues[2]
                keyPassword = releaseSigningValues[3]
            }
        }
    }
    buildTypes {
        getByName("release") { if (releaseSigningConfigured) signingConfig = signingConfigs.getByName("release") }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    testOptions {
        unitTests.all {
            it.systemProperty("jna.library.path", rootProject.file("target/debug").absolutePath)
            it.jvmArgs(
                "--add-opens=java.base/java.lang=ALL-UNNAMED",
                // UniFFI uses Android SystemCleaner in Robolectric API 35.
                "--add-exports=java.base/jdk.internal.ref=ALL-UNNAMED",
                "--add-opens=java.base/java.util=ALL-UNNAMED",
                "--add-opens=java.base/java.io=ALL-UNNAMED",
                "--add-opens=java.base/java.net=ALL-UNNAMED",
                "--add-opens=java.base/java.security=ALL-UNNAMED",
                "--add-opens=java.base/java.text=ALL-UNNAMED",
                "--add-opens=java.base/jdk.internal.access=ALL-UNNAMED",
                "--add-opens=java.desktop/java.awt.font=ALL-UNNAMED",
                "--add-opens=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
            )
        }
    }
    sourceSets.getByName("main") {
        kotlin.srcDir(rootProject.file("target/agent-bindings/dev"))
        res.srcDir("native-res")
        jniLibs.srcDir(layout.buildDirectory.dir("generated/jniLibs").get().asFile)
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    // JVM tests require the host JNA dispatcher; the app keeps the Android AAR.
    testImplementation("net.java.dev.jna:jna:5.19.1")
    testImplementation("org.robolectric:robolectric:4.17")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    implementation(project(":terminal-native"))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("androidx.lifecycle:lifecycle-process:2.9.4")
    implementation("com.revenuecat.purchases:placeholder:1.0.4")
    implementation("org.jetbrains.compose.foundation:foundation:1.11.1")
    implementation("org.jetbrains.compose.material3:material3:1.11.0-alpha07")
    implementation("org.jetbrains.compose.material:material-icons-extended:1.7.3")
    implementation("org.jetbrains.compose.ui:ui:1.11.1")
    implementation("androidx.camera:camera-camera2:1.6.1")
    implementation("androidx.camera:camera-lifecycle:1.6.1")
    implementation("androidx.camera:camera-view:1.6.1")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
    implementation("com.google.firebase:firebase-messaging:24.1.2")
    // Project icons may be SVG, which the platform image decoders do not read.
    implementation("com.caverock:androidsvg-aar:1.4")
}
