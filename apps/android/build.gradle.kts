import org.gradle.api.tasks.Exec

plugins { id("com.android.application") }

android {
    namespace = "dev.remoteagent.mobile"
    compileSdk = 37
    buildToolsVersion = "37.0.0"

    defaultConfig {
        applicationId = "dev.remoteagent.mobile"
        minSdk = 37
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
    }
}

dependencies { implementation(project(":apps:mobile")) }

val buildMobileClientAndroid by
    tasks.registering(Exec::class) {
        workingDir(rootProject.projectDir)
        environment("RUSTC", providers.environmentVariable("MOBILE_RUSTC").orElse("rustc").get())
        commandLine(
            providers.environmentVariable("MOBILE_CARGO").orElse("cargo").get(),
            "ndk",
            "--target",
            "arm64-v8a",
            "--target",
            "x86_64",
            "--output-dir",
            project.file("src/main/jniLibs").absolutePath,
            "build",
            "--package",
            "mobile-client",
            "--release",
            "--features",
            "jni",
        )
        inputs.files(rootProject.file("Cargo.toml"), rootProject.file("Cargo.lock"))
        for (crate in
            listOf("agent-client", "conversation-presentation", "host-protocol", "relay-transport", "mobile-client")) {
            inputs.dir(rootProject.file("crates/$crate"))
        }
        outputs.files(
            project.file("src/main/jniLibs/arm64-v8a/libmobile_client.so"),
            project.file("src/main/jniLibs/x86_64/libmobile_client.so"),
        )
    }

tasks
    .matching { task -> task.name.matches(Regex("merge.*JniLibFolders")) }
    .configureEach { dependsOn(buildMobileClientAndroid) }
