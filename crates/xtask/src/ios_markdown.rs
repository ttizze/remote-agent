//! Reuse Swift compilation while exercising the current Rust FFI on every run.
use crate::{
    Result, args,
    supervision::{self, Io},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

pub async fn run() -> Result<()> {
    if std::env::consts::OS != "macos" {
        return Err("iOS Markdown tests require Xcode on macOS".into());
    }
    let cancel = supervision::cancellation();
    let cwd = std::env::current_dir()?;
    supervision::run(
        &args!["scripts/build-agent-bindings.sh"],
        &cwd,
        Io::Inherit,
        &cancel,
        Duration::from_secs(3600),
    )
    .await?;
    let metadata = supervision::run(
        &args!["cargo", "metadata", "--no-deps", "--format-version", "1"],
        &cwd,
        Io::Capture,
        &cancel,
        Duration::from_secs(60),
    )
    .await?;
    let metadata: Value = serde_json::from_slice(&metadata.stdout)?;
    let target = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("missing Cargo target directory")?,
    );
    let bindings = cwd.join("target/agent-bindings");
    let mut hash = Sha256::new();
    for path in [
        bindings.join("AgentCore.swift"),
        bindings.join("AgentCoreFFI.h"),
        bindings.join("AgentCoreFFI.modulemap"),
        cwd.join("apps/mobile/iosApp/Bex/ConversationMarkdownContent.swift"),
        cwd.join("apps/mobile/iosApp/BexUITests/Fixtures/markdown-tests.swift"),
    ] {
        let bytes = fs::read(path)?;
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    hash.update(include_str!("ios_markdown.rs"));
    for arguments in [
        args![vec; "xcrun", "swiftc", "--version"],
        args![vec; "xcrun", "--show-sdk-build-version"],
        args![vec; "xcrun", "--show-sdk-path"],
    ] {
        let output = supervision::run(
            &arguments,
            &cwd,
            Io::Capture,
            &cancel,
            Duration::from_secs(60),
        )
        .await?;
        hash.update((output.stdout.len() as u64).to_le_bytes());
        hash.update(output.stdout);
    }
    for value in [
        target.to_string_lossy().into_owned(),
        bindings.to_string_lossy().into_owned(),
        std::env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_default(),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    let parent = target.join("qa/ios-markdown");
    let cache = parent.join(format!("{:x}", hash.finalize()));
    let executable = cache.join("markdown-tests");
    let reusable = executable
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
    if reusable {
        println!("Reusing unchanged iOS Markdown test build");
    } else {
        fs::create_dir_all(&parent)?;
        let temporary = tempfile::Builder::new()
            .prefix(".build.")
            .tempdir_in(&parent)?;
        let build = temporary.path();
        let module_map = format!(
            "-fmodule-map-file={}",
            bindings.join("AgentCoreFFI.modulemap").display()
        );
        for arguments in [
            args![vec;
                "xcrun",
                "swiftc",
                bindings.join("AgentCore.swift"),
                "-emit-library",
                "-emit-module",
                "-module-name",
                "AgentCore",
                "-emit-module-path",
                build.join("AgentCore.swiftmodule"),
                "-I",
                &bindings,
                "-Xcc",
                &module_map,
                "-L",
                target.join("debug"),
                "-lagent_ffi",
                "-o",
                build.join("libAgentCore.dylib"),
                "-Xlinker",
                "-install_name",
                "-Xlinker",
                "@rpath/libAgentCore.dylib"
            ],
            args![vec;
                "xcrun",
                "swiftc",
                "apps/mobile/iosApp/Bex/ConversationMarkdownContent.swift",
                "apps/mobile/iosApp/BexUITests/Fixtures/markdown-tests.swift",
                "-I",
                build,
                "-I",
                &bindings,
                "-Xcc",
                &module_map,
                "-L",
                build,
                "-lAgentCore",
                "-Xlinker",
                "-rpath",
                "-Xlinker",
                &cache,
                "-Xlinker",
                "-rpath",
                "-Xlinker",
                target.join("debug"),
                "-o",
                build.join("markdown-tests")
            ],
        ] {
            supervision::run(
                &arguments,
                &cwd,
                Io::Inherit,
                &cancel,
                Duration::from_secs(300),
            )
            .await?;
        }
        // Same-parent rename publishes a complete immutable build atomically.
        // Identical concurrent builders can use the complete winner.
        match fs::rename(build, &cache) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    supervision::run(
        &args![
            &executable,
            "crates/agent-core/tests/fixtures/markdown/table.md",
            "crates/agent-core/tests/fixtures/markdown/document.md"
        ],
        &cwd,
        Io::Inherit,
        &cancel,
        Duration::from_secs(60),
    )
    .await?;
    Ok(())
}
