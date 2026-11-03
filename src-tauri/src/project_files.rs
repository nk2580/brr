use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub fn validate_directory(path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() || path.chars().any(char::is_control) {
        return Err("Choose a valid project folder path".into());
    }
    let path = Path::new(path);
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("Cannot access project folder: {error}"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("Cannot inspect project folder: {error}"))?;
    if !metadata.is_dir() {
        return Err("The project path must be a directory".into());
    }
    Ok(canonical)
}

fn normalize_windows_explorer_path(path: &str) -> Result<String, String> {
    const VERBATIM_PREFIX: &str = "\\\\?\\";
    const VERBATIM_UNC_PREFIX: &str = "\\\\?\\UNC\\";

    if path
        .get(..VERBATIM_UNC_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(VERBATIM_UNC_PREFIX))
    {
        return Ok(format!("\\\\{}", &path[VERBATIM_UNC_PREFIX.len()..]));
    }
    if let Some(path) = path.strip_prefix(VERBATIM_PREFIX) {
        let is_drive_path = path.as_bytes().get(1) == Some(&b':');
        if is_drive_path {
            return Ok(path.to_owned());
        }
        return Err("Unsupported Windows project folder path".into());
    }
    Ok(path.to_owned())
}

fn reveal_command(path: &Path) -> Result<(OsString, Vec<OsString>), String> {
    #[cfg(target_os = "macos")]
    {
        Ok((
            "open".into(),
            vec!["-R".into(), path.as_os_str().to_owned()],
        ))
    }
    #[cfg(target_os = "windows")]
    {
        let path = path.to_str().ok_or_else(|| {
            "The project folder path cannot be represented for File Explorer".to_owned()
        })?;
        let path = normalize_windows_explorer_path(path)?;
        Ok((
            "explorer.exe".into(),
            vec![format!("/select,{path}").into()],
        ))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(("xdg-open".into(), vec![path.as_os_str().to_owned()]))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = path;
        Err("Opening folders in the file manager is unsupported on this platform".into())
    }
}

async fn launch(program: &std::ffi::OsStr, args: &[OsString]) -> Result<(), String> {
    let status = tokio::process::Command::new(program)
        .args(args)
        .status()
        .await
        .map_err(|error| format!("Could not open project folder in the file manager: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open project folder in the file manager".into())
    }
}

#[tauri::command]
pub async fn reveal_project_folder(path: String) -> Result<(), String> {
    let path = validate_directory(&path)?;
    let (program, args) = reveal_command(&path)?;
    launch(&program, &args).await
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

    #[test]
    fn creates_native_reveal_command_with_path_as_a_distinct_argument() {
        let path = std::path::Path::new("/tmp/project folder-λ & notes");
        let (program, args) = reveal_command(path).unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(
            (program, args),
            (
                "open".into(),
                vec!["-R".into(), path.as_os_str().to_owned()]
            )
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            (program, args),
            (
                "explorer.exe".into(),
                vec![format!("/select,{}", path.display()).into()]
            )
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            (program, args),
            ("xdg-open".into(), vec![path.as_os_str().to_owned()])
        );
    }

    #[test]
    fn removes_windows_verbatim_prefixes_before_building_explorer_argv() {
        assert_eq!(
            normalize_windows_explorer_path(r"\\?\C:\work\Brr λ & notes").unwrap(),
            r"C:\work\Brr λ & notes"
        );
        assert_eq!(
            normalize_windows_explorer_path(r"\\?\UNC\server\share\Brr λ").unwrap(),
            r"\\server\share\Brr λ"
        );
        assert_eq!(
            normalize_windows_explorer_path(r"D:\ordinary path").unwrap(),
            r"D:\ordinary path"
        );
    }

    #[tokio::test]
    async fn reports_process_start_and_nonzero_exit_errors() {
        assert!(
            launch(std::ffi::OsStr::new("brr-no-such-file-manager"), &[])
                .await
                .unwrap_err()
                .contains("Could not open project folder")
        );
        #[cfg(unix)]
        assert!(
            launch(std::ffi::OsStr::new("sh"), &["-c".into(), "exit 7".into()])
                .await
                .unwrap_err()
                .contains("Could not open project folder")
        );
    }
}
