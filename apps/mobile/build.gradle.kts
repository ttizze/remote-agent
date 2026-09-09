import org.gradle.api.tasks.Exec
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("io.gitlab.arturbosch.detekt")
    id("org.jetbrains.kotlin.multiplatform")
    id("com.android.kotlin.multiplatform.library")
    id("org.jetbrains.compose")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
    // ktfmt detects the Android KMP plugin when it is applied.
    id("com.ncorti.ktfmt.gradle")
}

ktfmt {
    kotlinLangStyle()
    maxWidth.set(120)
}

// Filter before traversal so Native compiler scratch files cannot race script discovery.
tasks.withType<com.ncorti.ktfmt.gradle.tasks.KtfmtBaseTask>().configureEach {
    if (name.endsWith("Scripts")) setSource(fileTree(projectDir) { include("*.kts") })
}

detekt {
    toolVersion = "1.23.8"
    buildUponDefaultConfig = true
    config.setFrom(rootProject.file("detekt.yml"))
    // Include Native and shared sources; the default JVM source paths miss KMP.
    source.setFrom("src")
    basePath = rootProject.projectDir.absolutePath
}

val mobileCargo = providers.environmentVariable("MOBILE_CARGO").orElse("cargo")
val mobileRustc = providers.environmentVariable("MOBILE_RUSTC").orElse("rustc")

kotlin {
    android {
        namespace = "dev.remoteagent.mobile.shared"
        compileSdk = 37
        minSdk = 37
        compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
        withHostTest {}
    }

    listOf(iosArm64() to "aarch64-apple-ios", iosSimulatorArm64() to "aarch64-apple-ios-sim").forEach {
        (target, rustTarget) ->
        val buildRustLibrary =
            tasks.register<Exec>("buildMobileClient${target.name.replaceFirstChar(Char::uppercaseChar)}") {
                workingDir(rootProject.projectDir)
                environment("RUSTC", mobileRustc.get())
                if (System.getProperty("os.name") == "Mac OS X") {
                    environment("CC", providers.environmentVariable("MOBILE_CC").orElse("/usr/bin/clang").get())
                    environment("CXX", providers.environmentVariable("MOBILE_CXX").orElse("/usr/bin/clang++").get())
                    environment("CARGO_TARGET_AARCH64_APPLE_IOS_LINKER", "/usr/bin/clang")
                    environment("CARGO_TARGET_AARCH64_APPLE_IOS_SIM_LINKER", "/usr/bin/clang")
                    environment("IPHONEOS_DEPLOYMENT_TARGET", "26.0")
                }
                commandLine(
                    mobileCargo.get(),
                    "build",
                    "--package",
                    "mobile-client",
                    "--release",
                    "--target",
                    rustTarget,
                )
                inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
                inputs.dir(rootProject.file("crates/agent-client"))
                inputs.dir(rootProject.file("crates/conversation-presentation"))
                inputs.dir(rootProject.file("crates/host-protocol"))
                inputs.dir(rootProject.file("crates/relay-transport"))
                inputs.dir(rootProject.file("crates/mobile-client"))
                outputs.file(rootProject.file("target/$rustTarget/release/libmobile_client.a"))
            }
        target.compilations.getByName("main").cinterops.create("mobileClient") {
            defFile(project.file("iosApp/Interop/mobile_client.def"))
            includeDirs.headerFilterOnly(rootProject.file("crates/mobile-client/include"))
        }
        target.binaries.framework {
            baseName = "RemoteAgentMobile"
            isStatic = true
        }
        target.binaries.withType<org.jetbrains.kotlin.gradle.plugin.mpp.TestExecutable> {
            linkerOpts(
                rootProject.file("target/$rustTarget/release/libmobile_client.a").absolutePath,
                "-framework",
                "Security",
                "-framework",
                "SystemConfiguration",
            )
        }
        target.binaries.all { linkTaskProvider.configure { dependsOn(buildRustLibrary) } }
    }

    sourceSets {
        commonMain.dependencies {
            implementation("org.jetbrains.compose.runtime:runtime:1.11.1")
            implementation("org.jetbrains.kotlinx:atomicfu:0.28.0")
            implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
            implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.11.0")
        }
        commonTest.dependencies { implementation(kotlin("test")) }
        getByName("androidHostTest").resources.srcDir(rootProject.file("crates/agent-client/tests/fixtures"))
        androidMain.dependencies {
            implementation("androidx.activity:activity-compose:1.12.4")
            implementation("org.jetbrains.compose.foundation:foundation:1.11.1")
            implementation("org.jetbrains.compose.material3:material3:1.11.0-alpha07")
            implementation("org.jetbrains.compose.ui:ui:1.11.1")
            implementation("androidx.camera:camera-camera2:1.6.1")
            implementation("androidx.camera:camera-lifecycle:1.6.1")
            implementation("androidx.camera:camera-view:1.6.1")
            implementation("com.google.mlkit:barcode-scanning:17.3.0")
        }
    }
}

// JVM unit tests execute the same JNI implementation as Android, using a host
// library rather than a Kotlin copy of the presentation rules.
val buildMobileClientJvmTests by
    tasks.registering(Exec::class) {
        workingDir(rootProject.projectDir)
        environment("RUSTC", mobileRustc.get())
        commandLine(mobileCargo.get(), "build", "--package", "mobile-client", "--features", "jni", "--lib")
        inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
        for (crate in
            listOf("agent-client", "conversation-presentation", "host-protocol", "relay-transport", "mobile-client")) {
            inputs.dir(rootProject.file("crates/$crate"))
        }
        outputs.file(rootProject.file("target/debug/" + System.mapLibraryName("mobile_client")))
    }

tasks.withType<org.gradle.api.tasks.testing.Test>().configureEach {
    dependsOn(buildMobileClientJvmTests)
    systemProperty("java.library.path", rootProject.file("target/debug").absolutePath)
}
