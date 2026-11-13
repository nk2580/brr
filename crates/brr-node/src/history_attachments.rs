//! Reads require a saved submission grant or an exact origin-qualified manifest.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use brr_host::export::{HistoryAttachment, HistoryBlob};

const MAX_PREVIEW: usize = 2 * 1024 * 1024;
const MAX_CHUNK: u64 = 256 * 1024;

impl Service {
    pub(super) async fn local_attachment_response(
        &self,
        request: &Request,
    ) -> Result<Value, ApiError> {
        validate_request(request)?;
        if let Some(result) = self.historical_response(request, self.node_id()).await? {
            return Ok(result);
        }
        self.connect_runtime().await?;
        let pair = self
            .host
            .history_attachment(
                required(&request.params, "threadId")?,
                required(&request.params, "sha256")?,
            )
            .await
            .map_err(host_error)?
            .ok_or_else(|| {
                ApiError::new(
                    "not_found",
                    "This conversation has no saved attachment with that digest",
                )
            })?;
        attachment_response(request, pair).await
    }
}

pub(super) fn validate_request(request: &Request) -> Result<(), ApiError> {
    required(&request.params, "threadId")?;
    let digest = required(&request.params, "sha256")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ApiError::new(
            "invalid_request",
            "Attachment digest must be lowercase SHA-256",
        ));
    }
    if request.method == "attachment.read" {
        bounded_integer(&request.params, "offset", 0, u64::MAX)?;
        bounded_integer(&request.params, "length", MAX_CHUNK, MAX_CHUNK)?;
    }
    Ok(())
}

fn bounded_integer(params: &Value, key: &str, default: u64, max: u64) -> Result<u64, ApiError> {
    match params.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| *n <= max && (key != "length" || *n > 0))
            .ok_or_else(|| {
                ApiError::new(
                    "invalid_request",
                    format!("Attachment {key} must be a bounded nonnegative integer"),
                )
            }),
    }
}

pub(super) async fn attachment_response(
    request: &Request,
    (attachment, blob): (HistoryAttachment, HistoryBlob),
) -> Result<Value, ApiError> {
    validate_request(request)?;
    let method = request.method.clone();
    let offset = bounded_integer(&request.params, "offset", 0, u64::MAX)?;
    let length = bounded_integer(&request.params, "length", MAX_CHUNK, MAX_CHUNK)?;
    tokio::task::spawn_blocking(move || {
        let bytes = blob.read_verified_bytes().map_err(host_error)?;
        if method == "attachment.read" {
            let start = usize::try_from(offset).ok().filter(|n| *n <= bytes.len())
                .ok_or_else(|| ApiError::new("invalid_request", "Attachment offset is out of range"))?;
            let end = start.saturating_add(length as usize).min(bytes.len());
            return Ok(json!({"attachment":attachment,"dataBase64":STANDARD.encode(&bytes[start..end]),"offset":offset,"nextOffset":(end < bytes.len()).then_some(end)}));
        }
        if bytes.len() > MAX_PREVIEW {
            return Ok(json!({"attachment":attachment,"preview":null,"reason":"preview-too-large"}));
        }
        if attachment.kind == "image" {
            if !matches!(attachment.mime.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif" | "image/bmp" | "image/tiff" | "image/heic" | "image/heif") {
                return Ok(json!({"attachment":attachment,"preview":null,"reason":"preview-binary"}));
            }
            return Ok(json!({"preview":format!("data:{};base64,{}",attachment.mime,STANDARD.encode(bytes)),"attachment":attachment}));
        }
        match String::from_utf8(bytes) {
            Ok(text) if text.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t' | '\u{000c}')) => Ok(json!({"attachment":attachment,"preview":text})),
            _ => Ok(json!({"attachment":attachment,"preview":null,"reason":"preview-binary"})),
        }
    }).await.map_err(host_error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use brr_host::export::HistoryExport;

    #[tokio::test]
    async fn primary_reads_an_offline_owners_attachment_after_real_git_transfer() {
        let dir = tempfile::tempdir().unwrap();
        let primary = Service::open(ServiceConfig {
            data_dir: dir.path().join("primary"),
            runtime_path: dir.path().join("missing-runtime"),
            name: "Primary".into(),
            endpoint_hosts: vec!["localhost".into()],
        })
        .await
        .unwrap();
        let catalog = Catalog::init_primary(
            primary.host.data_dir(),
            primary.node_id(),
            primary.host.agent_store(),
        )
        .await
        .unwrap();
        let worker_dir = std::fs::canonicalize(dir.path()).unwrap().join("worker");
        let worker_store = brr_agent_core::AgentStore::open(worker_dir.join("engine/agents"))
            .await
            .unwrap();
        let worker = Catalog::init_node(
            &worker_dir,
            "offline-worker",
            &catalog.origin().unwrap(),
            None,
            worker_store,
        )
        .await
        .unwrap();
        let bytes = b"available after original owner goes away\n";
        let sha = format!("{:x}", Sha256::digest(bytes));
        let source = worker_dir.join("saved-file");
        std::fs::write(&source, bytes).unwrap();
        let attachment = HistoryAttachment {
            id: "file-1".into(),
            name: "saved.txt".into(),
            mime: "text/plain".into(),
            kind: "text".into(),
            size_bytes: bytes.len() as u64,
            sha256: sha.clone(),
            submission_id: "accepted".into(),
            turn_id: None,
            item_id: None,
        };
        worker.publish_history(&HistoryExport{schema_version:1,owner_origin:"local-runtime".into(),complete:true,threads:vec![json!({"thread":{"id":"worker-thread"},"turns":[],"origin":"local-runtime","attachments":[attachment]})],blobs:vec![HistoryBlob::from_verified_path(&source,sha.clone(),bytes.len() as u64).unwrap()],failures:vec![]}).unwrap();
        worker.synchronize().await.unwrap();
        catalog.synchronize().await.unwrap();
        drop(worker);
        std::fs::remove_dir_all(worker_dir).unwrap();
        *primary.catalog.lock().await = Some(catalog);
        let request = |owner: &str, method: &str, thread: &str| Request {
            id: format!("{owner}-{method}-{thread}"),
            channel: Channel::Sessions,
            method: method.into(),
            params: json!({"threadId":thread,"sha256":sha}),
            operation_id: None,
            expected_epoch: None,
            expected_revision: None,
            target_node_id: Some(owner.into()),
        };
        let read = primary
            .handle(
                request("offline-worker", "attachment.preview", "worker-thread"),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(read.error.is_none(), "{:?}", read.error);
        assert_eq!(
            read.result.unwrap()["preview"],
            String::from_utf8(bytes.to_vec()).unwrap()
        );
        let read = primary
            .handle(
                request("offline-worker", "attachment.read", "worker-thread"),
                ClientRole::Mobile,
                "phone",
            )
            .await;
        assert!(read.error.is_none(), "{:?}", read.error);
        assert_eq!(
            STANDARD
                .decode(read.result.unwrap()["dataBase64"].as_str().unwrap())
                .unwrap(),
            bytes
        );
        let thread = primary
            .handle(
                request("offline-worker", "read", "worker-thread"),
                ClientRole::Mobile,
                "phone",
            )
            .await
            .result
            .unwrap();
        assert_eq!(thread["ownerNodeId"], "offline-worker");
        assert_eq!(thread["attachments"][0]["sha256"], sha);
        assert_eq!(thread["historical"], true);
        for (owner, thread) in [
            (primary.node_id(), "worker-thread"),
            ("other-owner", "worker-thread"),
            ("offline-worker", "unrelated-thread"),
        ] {
            assert!(primary
                .handle(
                    request(owner, "attachment.read", thread),
                    ClientRole::Mobile,
                    "phone"
                )
                .await
                .error
                .is_some());
        }
        assert!(!primary.host.is_connected().await);
        primary.shutdown().await;
    }
}
