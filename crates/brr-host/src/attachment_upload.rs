use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachmentUploadMetadata {
    pub owner_id: String,
    pub upload_id: String,
    pub name: String,
    pub mime: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentUploadState {
    Pending,
    Committed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadRecord {
    pub metadata: AttachmentUploadMetadata,
    pub state: AttachmentUploadState,
    #[serde(default)]
    pub attachments: Vec<Value>,
}

pub(crate) fn validate_metadata(
    metadata: &AttachmentUploadMetadata,
    max_file_bytes: u64,
) -> Result<(), String> {
    validate_id(&metadata.owner_id)?;
    let uuid = uuid::Uuid::parse_str(&metadata.upload_id).map_err(|_| "Invalid upload ID")?;
    if uuid.hyphenated().to_string() != metadata.upload_id {
        return Err("Upload ID must be a canonical UUID".into());
    }
    if metadata.name.is_empty()
        || metadata.name.len() > 255
        || metadata
            .name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err("Invalid attachment name".into());
    }
    let parts: Vec<_> = metadata.mime.split('/').collect();
    let valid_token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
    };
    if metadata.mime.len() > 128
        || parts.len() != 2
        || !valid_token(parts[0])
        || !valid_token(parts[1])
    {
        return Err("Invalid attachment content type".into());
    }
    if metadata.size_bytes > max_file_bytes {
        return Err("Attachment exceeds 50 MiB".into());
    }
    if metadata.sha256.len() != 64
        || !metadata
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("Attachment SHA-256 must be lowercase hexadecimal".into());
    }
    Ok(())
}

pub(crate) fn validate_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || value.chars().any(|c| c.is_control() || c == '/' || c == '\\')
    {
        Err("Invalid attachment owner".into())
    } else {
        Ok(())
    }
}

pub(crate) fn validate_staged_path(root: &Path, upload_id: &str, path: &Path) -> Result<(), String> {
    let expected = root.join("uploads").join(format!("{upload_id}.part"));
    if path != expected {
        return Err("Staged attachment path is outside the upload store".into());
    }
    Ok(())
}
