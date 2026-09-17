//! Shared address policy for native browsers.
pub fn browser_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input == "about:blank" {
        return Ok(input.into());
    }
    if input.is_empty() {
        return Err("URL を入力してください".into());
    }
    let source = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&source).map_err(|_| "URL が不正です")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("http または https の URL を入力してください".into());
    }
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::browser_url;

    #[test]
    fn accepts_web_addresses_and_blank_but_rejects_privileged_schemes_and_credentials() {
        for (input, expected) in [
            (" example.com/path ", "https://example.com/path"),
            ("http://localhost:8080", "http://localhost:8080/"),
            ("about:blank", "about:blank"),
        ] {
            assert_eq!(browser_url(input).unwrap(), expected);
        }
        for input in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:secret@example.com",
            "https://",
        ] {
            assert!(browser_url(input).is_err(), "{input}");
        }
    }
}
