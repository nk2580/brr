use std::path::{Path, PathBuf};

pub fn validate_directory(path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() || path.chars().any(char::is_control) {
        return Err("Choose a valid project folder path".into());
    }
    let canonical = std::fs::canonicalize(Path::new(path))
        .map_err(|error| format!("Cannot access project folder: {error}"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("Cannot inspect project folder: {error}"))?;
    if !metadata.is_dir() {
        return Err("The project path must be a directory".into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonicalizes_existing_directories_and_rejects_files_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("project folder-λ & notes");
        std::fs::create_dir(&folder).unwrap();
        let file = dir.path().join("project.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(
            validate_directory(&folder.to_string_lossy()).unwrap(),
            std::fs::canonicalize(&folder).unwrap()
        );
        assert!(validate_directory(&file.to_string_lossy())
            .unwrap_err()
            .contains("directory"));
        assert!(validate_directory(&dir.path().join("missing").to_string_lossy()).is_err());
        assert!(validate_directory("https://example.com/project").is_err());
    }
}
