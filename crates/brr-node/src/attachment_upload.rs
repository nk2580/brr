//! Desktop-only, resumable staging for locally selected attachment bytes.
use brr_host::{AttachmentUploadMetadata, AttachmentUploadRecord, AttachmentUploadState, Host};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

const CHUNK_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AttachmentUploadManager {
    root: PathBuf,
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginParams {
    owner_id: String,
    upload_id: String,
    name: String,
    mime: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadParams {
    owner_id: String,
    upload_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChunkParams {
    owner_id: String,
    upload_id: String,
    offset: u64,
    data_base64: String,
}

impl AttachmentUploadManager {
    pub(crate) fn open(chat_root: &Path) -> Result<Self, String> {
        let root = chat_root.join("uploads");
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(&root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Attachment upload storage must be a real directory".into());
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let secured = fs::symlink_metadata(&root).map_err(|error| error.to_string())?;
        if secured.uid() != unsafe { libc::geteuid() } || secured.mode() & 0o077 != 0 {
            return Err("Attachment upload storage is not private to this user".into());
        }
        Ok(Self {
            root,
            locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn path(&self, upload_id: &str) -> Result<PathBuf, String> {
        let uuid = uuid::Uuid::parse_str(upload_id).map_err(|_| "Invalid upload ID")?;
        if uuid.hyphenated().to_string() != upload_id {
            return Err("Upload ID must be a canonical UUID".into());
        }
        Ok(self.root.join(format!("{upload_id}.part")))
    }

    async fn lock(&self, upload_id: &str) -> Result<OwnedMutexGuard<()>, String> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            locks
                .entry(upload_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        Ok(lock.lock_owned().await)
    }

    pub(crate) async fn dispatch(
        &self,
        host: &Host,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match method {
            "attachments.upload.begin" => {
                let input: BeginParams = serde_json::from_value(params)
                    .map_err(|_| "Invalid attachment upload metadata")?;
                let metadata = AttachmentUploadMetadata {
                    owner_id: input.owner_id,
                    upload_id: input.upload_id,
                    name: input.name,
                    mime: input.mime,
                    size_bytes: input.size_bytes,
                    sha256: input.sha256,
                };
                let _guard = self.lock(&metadata.upload_id).await?;
                let record = host.attachment_upload_begin(metadata.clone()).await?;
                if record.state == AttachmentUploadState::Pending {
                    self.ensure_stage(&metadata.upload_id)?;
                } else {
                    let _ = self.remove_stage(&metadata.upload_id);
                }
                self.with_offset(record)
            }
            "attachments.upload.status" => {
                let input: UploadParams = serde_json::from_value(params)
                    .map_err(|_| "Invalid attachment upload status request")?;
                let _guard = self.lock(&input.upload_id).await?;
                let record = host
                    .attachment_upload_status(&input.owner_id, &input.upload_id)
                    .await?
                    .ok_or("Attachment upload not found")?;
                if record.state == AttachmentUploadState::Committed {
                    let _ = self.remove_stage(&input.upload_id);
                }
                self.with_offset(record)
            }
            "attachments.upload.chunk" => {
                let input: ChunkParams = serde_json::from_value(params)
                    .map_err(|_| "Invalid attachment upload chunk")?;
                let _guard = self.lock(&input.upload_id).await?;
                let record = host
                    .attachment_upload_status(&input.owner_id, &input.upload_id)
                    .await?
                    .ok_or("Attachment upload not found")?;
                if record.state != AttachmentUploadState::Pending {
                    return Err("Attachment upload is not pending".into());
                }
                let bytes = decode_chunk(&input.data_base64)?;
                if bytes.is_empty() {
                    return Err("Attachment upload chunk cannot be empty".into());
                }
                let path = self.path(&input.upload_id)?;
                let received = append_idempotent_chunk(
                    &path,
                    input.offset,
                    &bytes,
                    record.metadata.size_bytes,
                )?;
                Ok(json!({"receivedBytes":received}))
            }
            "attachments.upload.commit" => {
                let input: UploadParams = serde_json::from_value(params)
                    .map_err(|_| "Invalid attachment upload commit")?;
                let _guard = self.lock(&input.upload_id).await?;
                let record = host
                    .attachment_upload_status(&input.owner_id, &input.upload_id)
                    .await?
                    .ok_or("Attachment upload not found")?;
                if record.state == AttachmentUploadState::Committed {
                    let _ = self.remove_stage(&input.upload_id);
                    return serde_json::to_value(record).map_err(|error| error.to_string());
                }
                let path = self.path(&input.upload_id)?;
                let metadata = regular_metadata(&path)?;
                if metadata.len() != record.metadata.size_bytes {
                    return Err("Attachment upload is incomplete".into());
                }
                let committed = host
                    .attachment_upload_commit(&input.owner_id, &input.upload_id, &path)
                    .await?;
                let _ = self.remove_stage(&input.upload_id);
                serde_json::to_value(committed).map_err(|error| error.to_string())
            }
            "attachments.upload.cancel" => {
                let input: UploadParams = serde_json::from_value(params)
                    .map_err(|_| "Invalid attachment upload cancellation")?;
                let _guard = self.lock(&input.upload_id).await?;
                let record = host
                    .attachment_upload_cancel(&input.owner_id, &input.upload_id)
                    .await?;
                let _ = self.remove_stage(&input.upload_id);
                serde_json::to_value(record).map_err(|error| error.to_string())
            }
            _ => Err("Unsupported attachment upload method".into()),
        }
    }

    fn ensure_stage(&self, upload_id: &str) -> Result<(), String> {
        let path = self.path(upload_id)?;
        match regular_metadata(&path) {
            Ok(_) => Ok(()),
            Err(_) => {
                let mut options = OpenOptions::new();
                options.write(true).read(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW);
                let file = options.open(path).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
                sync_directory(&self.root)
            }
        }
    }

    fn remove_stage(&self, upload_id: &str) -> Result<(), String> {
        let path = self.path(upload_id)?;
        match fs::remove_file(path) {
            Ok(()) => sync_directory(&self.root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn with_offset(&self, record: AttachmentUploadRecord) -> Result<Value, String> {
        let received_bytes = if record.state == AttachmentUploadState::Pending {
            let path = self.path(&record.metadata.upload_id)?;
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(error.to_string()),
                Ok(_) => {
                    let metadata = regular_metadata(&path)?;
                    if metadata.len() > record.metadata.size_bytes {
                        return Err("Staged attachment exceeds its reserved size".into());
                    }
                    metadata.len()
                }
            }
        } else {
            0
        };
        let mut value = serde_json::to_value(record).map_err(|error| error.to_string())?;
        value["receivedBytes"] = json!(received_bytes);
        Ok(value)
    }
}

fn decode_chunk(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > ((CHUNK_LIMIT + 2) / 3) * 4 + 4 {
        return Err("Attachment upload chunk exceeds 1 MiB".into());
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| "Invalid attachment upload bytes")?;
    if bytes.len() > CHUNK_LIMIT {
        return Err("Attachment upload chunk exceeds 1 MiB".into());
    }
    Ok(bytes)
}

fn regular_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let before = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !before.file_type().is_file() || before.file_type().is_symlink() {
        return Err("Staged attachment is not a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path).map_err(|error| error.to_string())?;
    let after = file.metadata().map_err(|error| error.to_string())?;
    if !after.is_file()
        || after.len() != before.len()
        || after.ino() != before.ino()
        || after.uid() != unsafe { libc::geteuid() }
        || after.mode() & 0o077 != 0
    {
        return Err("Staged attachment changed while opening".into());
    }
    Ok(after)
}

fn append_idempotent_chunk(
    path: &Path,
    offset: u64,
    bytes: &[u8],
    expected_size: u64,
) -> Result<u64, String> {
    let verified = regular_metadata(path)?;
    let current = verified.len();
    let end = offset.checked_add(bytes.len() as u64).ok_or("Invalid attachment chunk offset")?;
    if end > expected_size || current > expected_size || offset > current {
        return Err("Attachment upload chunk is out of order".into());
    }
    let mut file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOFOLLOW).open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if opened.ino() != verified.ino() || opened.uid() != verified.uid() || opened.mode() & 0o077 != 0 {
        return Err("Staged attachment changed while opening".into());
    }
    if offset < current {
        file.seek(SeekFrom::Start(offset)).map_err(|error| error.to_string())?;
        let overlap = (current - offset).min(bytes.len() as u64) as usize;
        let mut existing = vec![0; overlap];
        file.read_exact(&mut existing).map_err(|error| error.to_string())?;
        if existing != bytes[..overlap] {
            return Err("Repeated attachment upload chunk does not match stored bytes".into());
        }
        if overlap == bytes.len() {
            return Ok(current);
        }
        file.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
        file.write_all(&bytes[overlap..]).map_err(|error| error.to_string())?;
    } else {
        file.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
    }
    file.sync_all().map_err(|error| error.to_string())?;
    let next = file.metadata().map_err(|error| error.to_string())?.len();
    if next > expected_size {
        return Err("Staged attachment exceeds its reserved size".into());
    }
    Ok(next)
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brr_host::HostConfig;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    fn temp_stage() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("8d26dd6f-7218-47c5-a24c-04974cc46843.part");
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn chunk_retries_are_idempotent_and_reconcile_a_partial_tail() {
        let (_dir, path) = temp_stage();
        assert_eq!(append_idempotent_chunk(&path, 0, b"abcd", 16).unwrap(), 4);
        assert_eq!(append_idempotent_chunk(&path, 0, b"abcd", 16).unwrap(), 4);
        assert_eq!(append_idempotent_chunk(&path, 2, b"cdef", 16).unwrap(), 6);
        assert_eq!(fs::read(path).unwrap(), b"abcdef");
    }

    #[test]
    fn chunk_retries_reject_conflicting_bytes_gaps_and_oversize() {
        let (_dir, path) = temp_stage();
        assert_eq!(append_idempotent_chunk(&path, 0, b"abcd", 4).unwrap(), 4);
        assert!(append_idempotent_chunk(&path, 0, b"abce", 4).is_err());
        assert!(append_idempotent_chunk(&path, 5, b"x", 8).is_err());
        assert!(append_idempotent_chunk(&path, 4, b"x", 4).is_err());
        assert_eq!(fs::read(path).unwrap(), b"abcd");
    }

    #[cfg(unix)]
    #[test]
    fn staged_chunk_target_must_not_be_a_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual");
        let link = dir.path().join("8d26dd6f-7218-47c5-a24c-04974cc46843.part");
        fs::write(&actual, b"untouchable").unwrap();
        symlink(&actual, &link).unwrap();
        assert!(append_idempotent_chunk(&link, 0, b"overwrite", 32).is_err());
        assert_eq!(fs::read(actual).unwrap(), b"untouchable");
    }

    async fn open_test_host(data_dir: &Path) -> Host {
        Host::open(
            HostConfig {
                data_dir: data_dir.to_path_buf(),
                runtime_path: data_dir.join("runtime-not-started"),
            },
            Arc::new(|_| {}),
        )
        .await
        .unwrap()
    }

    fn upload_metadata(owner: &str, upload_id: &str, bytes: &[u8]) -> Value {
        json!({
            "ownerId":owner,
            "uploadId":upload_id,
            "name":"offline-note.txt",
            "mime":"text/plain",
            "sizeBytes":bytes.len(),
            "sha256":format!("{:x}", Sha256::digest(bytes)),
        })
    }

    #[tokio::test]
    async fn upload_resumes_after_host_and_manager_reopen_without_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let id = "f753e3a2-f7d0-46d2-b50f-f26e39cd252b";
        let bytes = b"abcdef";
        let host = open_test_host(&data).await;
        let manager = AttachmentUploadManager::open(
            &host.data_dir().join("engine/agents/chat"),
        )
        .unwrap();
        let begin = manager
            .dispatch(
                &host,
                "attachments.upload.begin",
                upload_metadata("thread-owner", id, bytes),
            )
            .await
            .unwrap();
        assert_eq!(begin["receivedBytes"], 0);
        let first = manager
            .dispatch(
                &host,
                "attachments.upload.chunk",
                json!({"ownerId":"thread-owner","uploadId":id,"offset":0,"dataBase64":STANDARD.encode(b"abc")}),
            )
            .await
            .unwrap();
        assert_eq!(first["receivedBytes"], 3);
        drop(manager);
        drop(host);

        let reopened_host = open_test_host(&data).await;
        let reopened = AttachmentUploadManager::open(
            &reopened_host.data_dir().join("engine/agents/chat"),
        )
        .unwrap();
        let status = reopened
            .dispatch(
                &reopened_host,
                "attachments.upload.status",
                json!({"ownerId":"thread-owner","uploadId":id}),
            )
            .await
            .unwrap();
        assert_eq!(status["receivedBytes"], 3);
        reopened
            .dispatch(
                &reopened_host,
                "attachments.upload.chunk",
                json!({"ownerId":"thread-owner","uploadId":id,"offset":3,"dataBase64":STANDARD.encode(b"def")}),
            )
            .await
            .unwrap();
        let committed = reopened
            .dispatch(
                &reopened_host,
                "attachments.upload.commit",
                json!({"ownerId":"thread-owner","uploadId":id}),
            )
            .await
            .unwrap();
        assert_eq!(committed["state"], "committed");
        let receipt = committed["attachments"].clone();
        let replay = reopened
            .dispatch(
                &reopened_host,
                "attachments.upload.status",
                json!({"ownerId":"thread-owner","uploadId":id}),
            )
            .await
            .unwrap();
        assert_eq!(replay["attachments"], receipt);
        assert!(!reopened.path(id).unwrap().exists());
        assert!(!reopened_host.is_connected().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stage_cleanup_failure_does_not_hide_durable_commit_receipt() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let id = "429193fc-f3c9-421d-8dcc-f2517b59c47d";
        let bytes = b"receipt survives cleanup";
        let host = open_test_host(&data).await;
        let manager = AttachmentUploadManager::open(&host.data_dir().join("engine/agents/chat")).unwrap();
        manager
            .dispatch(
                &host,
                "attachments.upload.begin",
                upload_metadata("thread-owner", id, bytes),
            )
            .await
            .unwrap();
        manager
            .dispatch(
                &host,
                "attachments.upload.chunk",
                json!({"ownerId":"thread-owner","uploadId":id,"offset":0,"dataBase64":STANDARD.encode(bytes)}),
            )
            .await
            .unwrap();
        fs::set_permissions(&manager.root, fs::Permissions::from_mode(0o500)).unwrap();
        let committed = manager
            .dispatch(
                &host,
                "attachments.upload.commit",
                json!({"ownerId":"thread-owner","uploadId":id}),
            )
            .await
            .unwrap();
        fs::set_permissions(&manager.root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(committed["state"], "committed");
        assert_eq!(committed["attachments"].as_array().unwrap().len(), 1);
        let status = host
            .attachment_upload_status("thread-owner", id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.attachments, committed["attachments"].as_array().unwrap().clone());
    }
}
