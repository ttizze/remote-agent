pub fn validate_ssh_destination(destination: &str) -> Result<&str, &'static str> {
    let destination = destination.trim();
    if destination.is_empty()
        || destination.starts_with('-')
        || !destination.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '[' | ']' | ':')
        })
    {
        return Err("SSH の接続先は user@hostname の形式で入力してください。");
    }
    Ok(destination)
}
