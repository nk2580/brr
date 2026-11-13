use brr_remote_protocol::ApiError;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

pub struct InstanceLock {
    _file: File,
    directory: PathBuf,
}

impl InstanceLock {
    pub fn acquire(directory: &Path) -> Result<Self, ApiError> {
        private_directory(directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(directory.join("brrd.lock"))
            .map_err(io_error)?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
            ApiError::new(
                "already_running",
                "A Brr service already owns this node's execution stores",
            )
        })?;
        Ok(Self {
            _file: file,
            directory: directory.to_owned(),
        })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

pub fn private_directory(path: &Path) -> Result<(), ApiError> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ApiError::new(
                "invalid_data_directory",
                "The node data directory must be a real directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(ApiError::new(
                    "invalid_data_directory",
                    "The node data directory must belong to the current user",
                ));
            }
        }
    } else {
        std::fs::create_dir_all(path).map_err(io_error)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    Ok(())
}

fn io_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new("service_io", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_service_cannot_acquire_the_same_execution_stores() {
        let data = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(data.path()).unwrap();
        assert!(InstanceLock::acquire(data.path()).is_err());
        drop(first);
        assert!(InstanceLock::acquire(data.path()).is_ok());
    }
}
