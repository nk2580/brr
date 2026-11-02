pub fn validate(url: &str) -> Result<(), String> {
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("Unsupported external URL".into());
    }
    let parsed = url::Url::parse(url).map_err(|_| "Invalid external URL")?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
        || !(parsed.scheme() == "https" || (parsed.scheme() == "http" && local))
    {
        return Err("Only HTTPS or local HTTP URLs can be opened".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opens_oauth_https_and_local_callbacks_but_rejects_other_schemes_and_credentials() {
        for url in [
            "https://accounts.example.com/authorize?state=abc",
            "http://127.0.0.1:8080/auth",
            "http://[::1]:8080/auth",
        ] {
            assert!(validate(url).is_ok(), "{url}");
        }
        for url in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://name:pass@example.com/",
            "http://example.com/auth",
            "https://example.com/\n",
        ] {
            assert!(validate(url).is_err(), "{url}");
        }
    }
}
