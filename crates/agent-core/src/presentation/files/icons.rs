//! File glyph resolution from T3 Code's pierre-icons.ts and @pierre/trees 1.0.0-beta.6.
//! Sprite artwork and third-party terms are retained in apps/desktop/assets/file-icons.

/// Resolve a literal filename. Markdown positions are removed by the link parser,
/// so real filenames containing colons retain their own icon classification.
pub fn desktop_file_icon(path: &str) -> &'static str {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    if let Some(icon) = lookup(CUSTOM_NAMES, &name) {
        return icon;
    }
    let mut extensions =
        std::iter::successors(name.split_once('.').map(|(_, suffix)| suffix), |suffix| {
            suffix.split_once('.').map(|(_, suffix)| suffix)
        });
    for extension in extensions.clone() {
        if let Some(icon) = lookup(CUSTOM_EXTENSIONS, extension) {
            return icon;
        }
    }
    if let Some(icon) = lookup(BUILTIN_NAMES, &name) {
        return icon;
    }
    extensions
        .find_map(|extension| lookup(BUILTIN_EXTENSIONS, extension))
        .unwrap_or("file-tree-builtin-default")
}
fn lookup(rules: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    rules
        .binary_search_by_key(&key, |(name, _)| name)
        .ok()
        .map(|index| rules[index].1)
}

/// Unknown filenames alone may use a sniffed MIME type; a recognized name wins.
pub fn desktop_metadata_icon(path: &str, mime_type: Option<&str>) -> &'static str {
    let icon = desktop_file_icon(path);
    if icon != "file-tree-builtin-default" {
        return icon;
    }
    let path = match mime_type {
        Some("image/png") => "file.png",
        Some("image/jpeg") => "file.jpg",
        Some("image/gif") => "file.gif",
        Some("image/webp") => "file.webp",
        Some("application/pdf") => "file.pdf",
        Some("application/zip") => "file.zip",
        Some("application/vnd.sqlite3") => "file.sqlite",
        Some("text/x-python") => "file.py",
        Some("text/javascript") => "file.js",
        Some("text/x-ruby") => "file.rb",
        Some("text/x-shellscript") => "file.sh",
        _ => return icon,
    };
    desktop_file_icon(path)
}

const CUSTOM_NAMES: &[(&str, &str)] = &[
    ("agents.md", "t3-file-icon-agents"),
    ("package.json", "file-tree-builtin-npm"),
    ("pnpm-lock.yaml", "t3-file-icon-pnpm"),
    ("pnpm-workspace.yaml", "t3-file-icon-pnpm"),
    ("tsconfig.json", "file-tree-builtin-typescript"),
];

const CUSTOM_EXTENSIONS: &[(&str, &str)] = &[
    ("avi", "t3-file-icon-video"),
    ("cs", "t3-file-icon-csharp"),
    ("csv", "t3-file-icon-spreadsheet"),
    ("csx", "t3-file-icon-csharp"),
    ("dart", "t3-file-icon-dart"),
    ("diff", "t3-file-icon-diff"),
    ("doc", "t3-file-icon-word"),
    ("docx", "t3-file-icon-word"),
    ("epub", "t3-file-icon-epub"),
    ("ex", "t3-file-icon-elixir"),
    ("exs", "t3-file-icon-elixir"),
    ("heic", "file-tree-builtin-image"),
    ("heif", "file-tree-builtin-image"),
    ("hs", "t3-file-icon-haskell"),
    ("java", "t3-file-icon-java"),
    ("kt", "t3-file-icon-kotlin"),
    ("kts", "t3-file-icon-kotlin"),
    ("lhs", "t3-file-icon-haskell"),
    ("lua", "t3-file-icon-lua"),
    ("luau", "t3-file-icon-lua"),
    ("m4v", "t3-file-icon-video"),
    ("mkv", "t3-file-icon-video"),
    ("mov", "t3-file-icon-video"),
    ("mp4", "t3-file-icon-video"),
    ("odp", "t3-file-icon-slides"),
    ("ods", "t3-file-icon-spreadsheet"),
    ("odt", "t3-file-icon-word"),
    ("ogv", "t3-file-icon-video"),
    ("patch", "t3-file-icon-diff"),
    ("pdf", "t3-file-icon-pdf"),
    ("php", "t3-file-icon-php"),
    ("ppt", "t3-file-icon-slides"),
    ("pptx", "t3-file-icon-slides"),
    ("ps1", "t3-file-icon-powershell"),
    ("psd1", "t3-file-icon-powershell"),
    ("psm1", "t3-file-icon-powershell"),
    ("r", "t3-file-icon-r"),
    ("rtf", "t3-file-icon-word"),
    ("scala", "t3-file-icon-scala"),
    ("toml", "t3-file-icon-toml"),
    ("tsv", "t3-file-icon-spreadsheet"),
    ("webm", "t3-file-icon-video"),
    ("xls", "t3-file-icon-spreadsheet"),
    ("xlsx", "t3-file-icon-spreadsheet"),
    ("xml", "t3-file-icon-xml"),
    ("xsd", "t3-file-icon-xml"),
    ("xsl", "t3-file-icon-xml"),
    ("xslt", "t3-file-icon-xml"),
];

const BUILTIN_NAMES: &[(&str, &str)] = &[
    (".babelrc", "file-tree-builtin-babel"),
    (".babelrc.json", "file-tree-builtin-babel"),
    (".bash_profile", "file-tree-builtin-bash"),
    (".bashrc", "file-tree-builtin-bash"),
    (".browserslistrc", "file-tree-builtin-browserslist"),
    (".dockerignore", "file-tree-builtin-docker"),
    (".eslintignore", "file-tree-builtin-eslint"),
    (".eslintrc", "file-tree-builtin-eslint"),
    (".eslintrc.cjs", "file-tree-builtin-eslint"),
    (".eslintrc.js", "file-tree-builtin-eslint"),
    (".eslintrc.json", "file-tree-builtin-eslint"),
    (".eslintrc.yaml", "file-tree-builtin-eslint"),
    (".eslintrc.yml", "file-tree-builtin-eslint"),
    (".gitattributes", "file-tree-builtin-git"),
    (".gitignore", "file-tree-builtin-git"),
    (".gitkeep", "file-tree-builtin-git"),
    (".gitmodules", "file-tree-builtin-git"),
    (".oxlintrc.json", "file-tree-builtin-oxc"),
    (".postcssrc", "file-tree-builtin-postcss"),
    (".postcssrc.json", "file-tree-builtin-postcss"),
    (".postcssrc.yaml", "file-tree-builtin-postcss"),
    (".postcssrc.yml", "file-tree-builtin-postcss"),
    (".prettierignore", "file-tree-builtin-prettier"),
    (".prettierrc", "file-tree-builtin-prettier"),
    (".prettierrc.cjs", "file-tree-builtin-prettier"),
    (".prettierrc.js", "file-tree-builtin-prettier"),
    (".prettierrc.json", "file-tree-builtin-prettier"),
    (".prettierrc.mjs", "file-tree-builtin-prettier"),
    (".prettierrc.yaml", "file-tree-builtin-prettier"),
    (".prettierrc.yml", "file-tree-builtin-prettier"),
    (".stylelintignore", "file-tree-builtin-stylelint"),
    (".stylelintrc", "file-tree-builtin-stylelint"),
    (".stylelintrc.cjs", "file-tree-builtin-stylelint"),
    (".stylelintrc.js", "file-tree-builtin-stylelint"),
    (".stylelintrc.json", "file-tree-builtin-stylelint"),
    (".stylelintrc.mjs", "file-tree-builtin-stylelint"),
    (".stylelintrc.yaml", "file-tree-builtin-stylelint"),
    (".stylelintrc.yml", "file-tree-builtin-stylelint"),
    (".terraform.lock.hcl", "file-tree-builtin-terraform"),
    (".zprofile", "file-tree-builtin-bash"),
    (".zshenv", "file-tree-builtin-bash"),
    (".zshrc", "file-tree-builtin-bash"),
    ("babel.config.cjs", "file-tree-builtin-babel"),
    ("babel.config.js", "file-tree-builtin-babel"),
    ("babel.config.json", "file-tree-builtin-babel"),
    ("babel.config.mjs", "file-tree-builtin-babel"),
    ("biome.json", "file-tree-builtin-biome"),
    ("biome.jsonc", "file-tree-builtin-biome"),
    ("bootstrap.bundle.js", "file-tree-builtin-bootstrap"),
    ("bootstrap.bundle.min.js", "file-tree-builtin-bootstrap"),
    ("bootstrap.css", "file-tree-builtin-bootstrap"),
    ("bootstrap.js", "file-tree-builtin-bootstrap"),
    ("bootstrap.min.css", "file-tree-builtin-bootstrap"),
    ("bootstrap.min.js", "file-tree-builtin-bootstrap"),
    ("bun.lock", "file-tree-builtin-bun"),
    ("bun.lockb", "file-tree-builtin-bun"),
    ("claude.md", "file-tree-builtin-claude"),
    ("compose.yaml", "file-tree-builtin-docker"),
    ("compose.yml", "file-tree-builtin-docker"),
    ("docker-compose.override.yml", "file-tree-builtin-docker"),
    ("docker-compose.yaml", "file-tree-builtin-docker"),
    ("docker-compose.yml", "file-tree-builtin-docker"),
    ("dockerfile", "file-tree-builtin-docker"),
    ("eslint.config.cjs", "file-tree-builtin-eslint"),
    ("eslint.config.js", "file-tree-builtin-eslint"),
    ("eslint.config.mjs", "file-tree-builtin-eslint"),
    ("eslint.config.mts", "file-tree-builtin-eslint"),
    ("eslint.config.ts", "file-tree-builtin-eslint"),
    ("gemfile", "file-tree-builtin-ruby"),
    ("next.config.js", "file-tree-builtin-nextjs"),
    ("next.config.mjs", "file-tree-builtin-nextjs"),
    ("next.config.mts", "file-tree-builtin-nextjs"),
    ("next.config.ts", "file-tree-builtin-nextjs"),
    ("postcss.config.cjs", "file-tree-builtin-postcss"),
    ("postcss.config.js", "file-tree-builtin-postcss"),
    ("postcss.config.mjs", "file-tree-builtin-postcss"),
    ("postcss.config.ts", "file-tree-builtin-postcss"),
    ("prettier.config.cjs", "file-tree-builtin-prettier"),
    ("prettier.config.js", "file-tree-builtin-prettier"),
    ("prettier.config.mjs", "file-tree-builtin-prettier"),
    ("rakefile", "file-tree-builtin-ruby"),
    ("readme.md", "file-tree-builtin-markdown"),
    ("stylelint.config.cjs", "file-tree-builtin-stylelint"),
    ("stylelint.config.js", "file-tree-builtin-stylelint"),
    ("stylelint.config.mjs", "file-tree-builtin-stylelint"),
    ("svgo.config.cjs", "file-tree-builtin-svgo"),
    ("svgo.config.js", "file-tree-builtin-svgo"),
    ("svgo.config.mjs", "file-tree-builtin-svgo"),
    ("svgo.config.ts", "file-tree-builtin-svgo"),
    ("tailwind.config.cjs", "file-tree-builtin-tailwind"),
    ("tailwind.config.js", "file-tree-builtin-tailwind"),
    ("tailwind.config.mjs", "file-tree-builtin-tailwind"),
    ("tailwind.config.ts", "file-tree-builtin-tailwind"),
    ("vite.config.js", "file-tree-builtin-vite"),
    ("vite.config.mjs", "file-tree-builtin-vite"),
    ("vite.config.mts", "file-tree-builtin-vite"),
    ("vite.config.ts", "file-tree-builtin-vite"),
    ("webpack.config.babel.js", "file-tree-builtin-webpack"),
    ("webpack.config.cjs", "file-tree-builtin-webpack"),
    ("webpack.config.js", "file-tree-builtin-webpack"),
    ("webpack.config.mjs", "file-tree-builtin-webpack"),
    ("webpack.config.ts", "file-tree-builtin-webpack"),
];

const BUILTIN_EXTENSIONS: &[(&str, &str)] = &[
    ("7z", "file-tree-builtin-zip"),
    ("astro", "file-tree-builtin-astro"),
    ("avif", "file-tree-builtin-image"),
    ("bash", "file-tree-builtin-bash"),
    ("bmp", "file-tree-builtin-image"),
    ("bz2", "file-tree-builtin-zip"),
    ("c", "file-tree-builtin-c"),
    ("cc", "file-tree-builtin-cpp"),
    ("cfg", "file-tree-builtin-text"),
    ("cjs", "file-tree-builtin-javascript"),
    ("code-workspace", "file-tree-builtin-vscode"),
    ("conf", "file-tree-builtin-text"),
    ("cpp", "file-tree-builtin-cpp"),
    ("csh", "file-tree-builtin-bash"),
    ("css", "file-tree-builtin-css"),
    ("cts", "file-tree-builtin-typescript"),
    ("cxx", "file-tree-builtin-cpp"),
    ("db", "file-tree-builtin-database"),
    ("editorconfig", "file-tree-builtin-text"),
    ("env", "file-tree-builtin-text"),
    ("env.development", "file-tree-builtin-text"),
    ("env.local", "file-tree-builtin-text"),
    ("env.production", "file-tree-builtin-text"),
    ("eot", "file-tree-builtin-font"),
    ("erb", "file-tree-builtin-ruby"),
    ("fish", "file-tree-builtin-bash"),
    ("gemspec", "file-tree-builtin-ruby"),
    ("gif", "file-tree-builtin-image"),
    ("go", "file-tree-builtin-go"),
    ("gql", "file-tree-builtin-graphql"),
    ("graphql", "file-tree-builtin-graphql"),
    ("gz", "file-tree-builtin-zip"),
    ("h", "file-tree-builtin-c"),
    ("hh", "file-tree-builtin-cpp"),
    ("hpp", "file-tree-builtin-cpp"),
    ("htm", "file-tree-builtin-html"),
    ("html", "file-tree-builtin-html"),
    ("hxx", "file-tree-builtin-cpp"),
    ("icns", "file-tree-builtin-image"),
    ("ico", "file-tree-builtin-image"),
    ("ini", "file-tree-builtin-text"),
    ("inl", "file-tree-builtin-cpp"),
    ("jar", "file-tree-builtin-zip"),
    ("jpeg", "file-tree-builtin-image"),
    ("jpg", "file-tree-builtin-image"),
    ("js", "file-tree-builtin-javascript"),
    ("json", "file-tree-builtin-json"),
    ("json5", "file-tree-builtin-json"),
    ("jsonc", "file-tree-builtin-json"),
    ("jsonl", "file-tree-builtin-json"),
    ("jsx", "file-tree-builtin-react"),
    ("ksh", "file-tree-builtin-bash"),
    ("less", "file-tree-builtin-css"),
    ("log", "file-tree-builtin-text"),
    ("markdown", "file-tree-builtin-markdown"),
    ("mcp", "file-tree-builtin-mcp"),
    ("md", "file-tree-builtin-markdown"),
    ("mdx", "file-tree-builtin-markdown"),
    ("mdx.tsx", "file-tree-builtin-markdown"),
    ("mjs", "file-tree-builtin-javascript"),
    ("mm", "file-tree-builtin-cpp"),
    ("mts", "file-tree-builtin-typescript"),
    ("otf", "file-tree-builtin-font"),
    ("png", "file-tree-builtin-image"),
    ("postcss", "file-tree-builtin-css"),
    ("py", "file-tree-builtin-python"),
    ("pyi", "file-tree-builtin-python"),
    ("pyw", "file-tree-builtin-python"),
    ("pyx", "file-tree-builtin-python"),
    ("rake", "file-tree-builtin-ruby"),
    ("rar", "file-tree-builtin-zip"),
    ("rb", "file-tree-builtin-ruby"),
    ("rs", "file-tree-builtin-rust"),
    ("rst", "file-tree-builtin-text"),
    ("sass", "file-tree-builtin-sass"),
    ("scss", "file-tree-builtin-sass"),
    ("sh", "file-tree-builtin-bash"),
    ("sql", "file-tree-builtin-database"),
    ("sqlite", "file-tree-builtin-database"),
    ("sqlite3", "file-tree-builtin-database"),
    ("styl", "file-tree-builtin-css"),
    ("svelte", "file-tree-builtin-svelte"),
    ("svg", "file-tree-builtin-svg"),
    ("swift", "file-tree-builtin-swift"),
    ("tar", "file-tree-builtin-zip"),
    ("tf", "file-tree-builtin-terraform"),
    ("tfstate", "file-tree-builtin-terraform"),
    ("tfvars", "file-tree-builtin-terraform"),
    ("tgz", "file-tree-builtin-zip"),
    ("tif", "file-tree-builtin-image"),
    ("tiff", "file-tree-builtin-image"),
    ("ts", "file-tree-builtin-typescript"),
    ("tsx", "file-tree-builtin-react"),
    ("ttf", "file-tree-builtin-font"),
    ("txt", "file-tree-builtin-text"),
    ("vue", "file-tree-builtin-vue"),
    ("war", "file-tree-builtin-zip"),
    ("wasm", "file-tree-builtin-wasm"),
    ("wast", "file-tree-builtin-wasm"),
    ("wat", "file-tree-builtin-wasm"),
    ("webp", "file-tree-builtin-image"),
    ("woff", "file-tree-builtin-font"),
    ("woff2", "file-tree-builtin-font"),
    ("xhtml", "file-tree-builtin-html"),
    ("xz", "file-tree-builtin-zip"),
    ("yaml", "file-tree-builtin-yml"),
    ("yml", "file-tree-builtin-yml"),
    ("zig", "file-tree-builtin-zig"),
    ("zip", "file-tree-builtin-zip"),
    ("zsh", "file-tree-builtin-bash"),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_only_refines_the_default_file_glyph() {
        for (mime, expected) in [
            ("image/png", "file-tree-builtin-image"),
            ("image/jpeg", "file-tree-builtin-image"),
            ("image/gif", "file-tree-builtin-image"),
            ("image/webp", "file-tree-builtin-image"),
            ("application/pdf", "t3-file-icon-pdf"),
            ("application/zip", "file-tree-builtin-zip"),
            ("application/vnd.sqlite3", "file-tree-builtin-database"),
            ("text/x-python", "file-tree-builtin-python"),
            ("text/javascript", "file-tree-builtin-javascript"),
            ("text/x-ruby", "file-tree-builtin-ruby"),
            ("text/x-shellscript", "file-tree-builtin-bash"),
            ("unknown", "file-tree-builtin-default"),
        ] {
            assert_eq!(desktop_metadata_icon("extensionless", Some(mime)), expected);
            assert_eq!(
                desktop_metadata_icon("package.json", Some(mime)),
                "file-tree-builtin-npm"
            );
        }
        assert_eq!(
            desktop_metadata_icon("extensionless", None),
            "file-tree-builtin-default"
        );
    }
    #[test]
    fn glyphs_follow_t3_name_extension_and_longest_suffix_precedence() {
        for (path, icon) in [
            ("src/AGENTS.md", "t3-file-icon-agents"),
            ("package.json", "file-tree-builtin-npm"),
            (r"C:\project\package.json", "file-tree-builtin-npm"),
            ("pnpm-lock.yaml", "t3-file-icon-pnpm"),
            ("Dockerfile", "file-tree-builtin-docker"),
            ("tsconfig.json", "file-tree-builtin-typescript"),
            ("tsconfig.app.json", "file-tree-builtin-json"),
            ("app.tsx", "file-tree-builtin-react"),
            ("page.mdx.tsx", "file-tree-builtin-markdown"),
            ("report.md.pdf", "t3-file-icon-pdf"),
            ("report.pdf.md", "file-tree-builtin-markdown"),
            (".gitignore", "file-tree-builtin-git"),
            (".rs", "file-tree-builtin-rust"),
            ("src/file..rs", "file-tree-builtin-rust"),
            ("a.PDF", "t3-file-icon-pdf"),
            ("a.csv", "t3-file-icon-spreadsheet"),
            ("a.kt", "t3-file-icon-kotlin"),
            ("a.MP4", "t3-file-icon-video"),
            ("a.rs:12", "file-tree-builtin-default"),
            ("unknown", "file-tree-builtin-default"),
        ] {
            assert_eq!(desktop_file_icon(path), icon, "{path}");
        }
        for rules in [
            CUSTOM_NAMES,
            CUSTOM_EXTENSIONS,
            BUILTIN_NAMES,
            BUILTIN_EXTENSIONS,
        ] {
            assert!(
                rules.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "lookup keys must stay unique and sorted"
            );
        }
    }
}
