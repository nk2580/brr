use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use brr_remote_protocol::{Event, EventCursor, EventReplay, Reply, Request};
use futures_util::StreamExt;
use keyring::Entry;
use reqwest::{Certificate, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, State};

mod sse;
use sse::SseDecoder;

const KEYCHAIN_SERVICE: &str = "dev.brr.mobile.device-credential";

#[derive(Default)]
struct MobileState {
    subscriptions: Mutex<HashMap<String, JoinHandle<()>>>,
    profiles: Mutex<()>,
}

enum ParsedRemoteSse {
    Event(Event),
    Replay(EventReplay),
}

fn parse_remote_sse(event_name: &str, data: &str) -> Result<Option<ParsedRemoteSse>, String> {
    match event_name {
        "event" | "message" => serde_json::from_str::<Event>(data)
            .map(ParsedRemoteSse::Event)
            .map(Some)
            .map_err(|error| format!("Invalid remote event frame: {error}")),
        "replay" | "snapshot_required" => serde_json::from_str::<EventReplay>(data)
            .map(ParsedRemoteSse::Replay)
            .map(Some)
            .map_err(|error| format!("Invalid remote replay frame: {error}")),
        _ => Ok(None),
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairInvitation {
    endpoint: String,
    code: String,
    fingerprint: String,
    certificate_pem: String,
    name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredProfile {
    id: String,
    name: String,
    endpoint: String,
    fingerprint: String,
    node_id: String,
    trusted_server_cert_pem: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicProfile {
    id: String,
    name: String,
    endpoint: String,
    fingerprint: String,
    node_id: String,
}

impl From<&StoredProfile> for PublicProfile {
    fn from(profile: &StoredProfile) -> Self {
        Self {
            id: profile.id.clone(),
            name: profile.name.clone(),
            endpoint: profile.endpoint.clone(),
            fingerprint: profile.fingerprint.clone(),
            node_id: profile.node_id.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairReply {
    device_id: String,
    secret: String,
    trusted_server_cert_pem: String,
    fingerprint: String,
    node_id: String,
    name: String,
    role: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest<'a> {
    code: &'a str,
    requested_role: &'static str,
    name: &'a str,
}

fn app_data(app: &AppHandle) -> Result<PathBuf, String> {
    let path = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(path)
}

fn profile_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_data(app)?.join("profiles.json"))
}
fn cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app_data(app)?.join("remote-cache");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(dir)
}

fn read_profiles(app: &AppHandle) -> Result<Vec<StoredProfile>, String> {
    let file = profile_file(app)?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&fs::read(file).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn write_profiles(app: &AppHandle, profiles: &[StoredProfile]) -> Result<(), String> {
    let file = profile_file(app)?;
    write_private_json(&file, profiles)
}

fn write_private_json(path: &Path, value: &(impl Serialize + ?Sized)) -> Result<(), String> {
    let encoded = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let temp = path.with_extension(format!("{}.pending", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|error| error.to_string())
}

fn profile(app: &AppHandle, id: &str) -> Result<StoredProfile, String> {
    read_profiles(app)?
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| "Paired node not found on this device".to_owned())
}

fn key_entry(id: &str) -> Result<Entry, String> {
    Entry::new(KEYCHAIN_SERVICE, id).map_err(|error| error.to_string())
}

fn credential(id: &str) -> Result<String, String> {
    key_entry(id)?
        .get_password()
        .map_err(|error| format!("Device credential is unavailable in iOS Keychain: {error}"))
}

fn cert_fingerprint(pem: &str) -> Result<String, String> {
    let encoded = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>();
    let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(|error| format!("Invalid trusted certificate: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(der)))
}

fn normalize_fingerprint(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .replace("sha256:", "")
        .replace(':', "")
        .replace(' ', "")
}

fn verify_certificate_pin(pem: &str, expected_fingerprint: &str) -> Result<String, String> {
    let fingerprint = cert_fingerprint(pem)?;
    if normalize_fingerprint(&fingerprint) != normalize_fingerprint(expected_fingerprint) {
        return Err("The invitation certificate does not match its fingerprint".into());
    }
    Ok(fingerprint)
}

fn endpoint_url(endpoint: &str, path: &str) -> Result<Url, String> {
    let mut base =
        Url::parse(endpoint).map_err(|error| format!("Invalid service endpoint: {error}"))?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || base.username() != ""
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || !matches!(base.path(), "" | "/")
    {
        return Err("Pairing requires a clean HTTPS endpoint with a verified hostname".into());
    }
    base.set_path(path);
    Ok(base)
}

fn pinned_client(pem: &str) -> Result<Client, String> {
    let root = Certificate::from_pem(pem.as_bytes())
        .map_err(|error| format!("Invalid trusted server certificate: {error}"))?;
    Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .tls_built_in_root_certs(false)
        .add_root_certificate(root)
        .build()
        .map_err(|error| error.to_string())
}

async fn authenticated_client(
    app: &AppHandle,
    profile_id: &str,
) -> Result<(StoredProfile, Client, String), String> {
    let profile = profile(app, profile_id)?;
    let token = credential(profile_id)?;
    let client = pinned_client(&profile.trusted_server_cert_pem)?;
    Ok((profile, client, token))
}

#[tauri::command]
async fn remote_pair(
    app: AppHandle,
    state: State<'_, MobileState>,
    invitation: PairInvitation,
    name: Option<String>,
) -> Result<PublicProfile, String> {
    verify_certificate_pin(&invitation.certificate_pem, &invitation.fingerprint)?;
    let client = pinned_client(&invitation.certificate_pem)?;
    let display_name = name
        .or(invitation.name)
        .unwrap_or_else(|| "Brr Mobile".into());
    let response = client
        .post(endpoint_url(&invitation.endpoint, "/v1/pair")?)
        .timeout(Duration::from_secs(30))
        .json(&PairRequest {
            code: &invitation.code,
            requested_role: "mobile",
            name: &display_name,
        })
        .send()
        .await
        .map_err(|error| format!("Could not reach the primary node: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Pairing was rejected by the primary node: {error}"))?
        .json::<PairReply>()
        .await
        .map_err(|error| format!("Invalid pairing response: {error}"))?;
    if response.role != "mobile" {
        return Err("The primary issued a credential with the wrong device role".into());
    }
    let certificate = if response.trusted_server_cert_pem.is_empty() {
        invitation.certificate_pem
    } else {
        response.trusted_server_cert_pem
    };
    let returned_fingerprint = cert_fingerprint(&certificate)?;
    if normalize_fingerprint(&returned_fingerprint)
        != normalize_fingerprint(&invitation.fingerprint)
        || normalize_fingerprint(&response.fingerprint)
            != normalize_fingerprint(&invitation.fingerprint)
    {
        return Err(
            "The primary returned a certificate that does not match the invitation fingerprint"
                .into(),
        );
    }
    if response.device_id.is_empty() || response.node_id.is_empty() || response.secret.is_empty() {
        return Err("The primary returned incomplete device credentials".into());
    }
    let stored = StoredProfile {
        id: response.device_id,
        name: if response.name.is_empty() {
            display_name
        } else {
            response.name
        },
        endpoint: invitation.endpoint,
        fingerprint: returned_fingerprint,
        node_id: response.node_id,
        trusted_server_cert_pem: certificate,
    };
    let _guard = state
        .profiles
        .lock()
        .map_err(|_| "Pairing storage is busy".to_owned())?;
    let mut profiles = read_profiles(&app)?;
    key_entry(&stored.id)?
        .set_password(&response.secret)
        .map_err(|error| format!("Could not store the device credential in Keychain: {error}"))?;
    profiles.retain(|profile| profile.id != stored.id);
    profiles.push(stored.clone());
    if let Err(error) = write_profiles(&app, &profiles) {
        let _ = key_entry(&stored.id)?.delete_credential();
        return Err(error);
    }
    Ok(PublicProfile::from(&stored))
}

#[tauri::command]
fn remote_profiles(app: AppHandle) -> Result<Vec<PublicProfile>, String> {
    Ok(read_profiles(&app)?
        .iter()
        .map(PublicProfile::from)
        .collect())
}

#[tauri::command]
async fn remote_forget(
    app: AppHandle,
    state: State<'_, MobileState>,
    id: String,
) -> Result<(), String> {
    let _guard = state
        .profiles
        .lock()
        .map_err(|_| "Pairing storage is busy".to_owned())?;
    let mut profiles = read_profiles(&app)?;
    if profiles.iter().all(|profile| profile.id != id) {
        return Ok(());
    }
    match key_entry(&id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => (),
        Err(error) => {
            return Err(format!(
                "Could not remove the device credential from Keychain: {error}"
            ))
        }
    }
    profiles.retain(|profile| profile.id != id);
    write_profiles(&app, &profiles)?;
    Ok(())
}

#[tauri::command]
async fn remote_request(
    app: AppHandle,
    profile_id: String,
    request: Request,
) -> Result<Reply, String> {
    let (profile, client, token) = authenticated_client(&app, &profile_id).await?;
    let url = endpoint_url(&profile.endpoint, "/v1/request")?;
    client.post(url).timeout(Duration::from_secs(30)).bearer_auth(token).json(&request).send().await
        .map_err(|error| format!("The remote command may have reached the node; inspect current state before retrying: {error}"))?
        .error_for_status().map_err(|error| error.to_string())?
        .json::<Reply>().await.map_err(|error| format!("The remote command response could not be confirmed: {error}"))
}

#[tauri::command]
async fn remote_snapshot(app: AppHandle, profile_id: String) -> Result<Value, String> {
    let (profile, client, token) = authenticated_client(&app, &profile_id).await?;
    client
        .get(endpoint_url(&profile.endpoint, "/v1/snapshot")?)
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json::<Value>()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn remote_subscribe(
    app: AppHandle,
    state: State<'_, MobileState>,
    profile_id: String,
    cursor: Option<EventCursor>,
) -> Result<(), String> {
    let (profile, client, token) = authenticated_client(&app, &profile_id).await?;
    let mut url = endpoint_url(&profile.endpoint, "/v1/events")?;
    if let Some(cursor) = cursor {
        url.query_pairs_mut()
            .append_pair("epoch", &cursor.epoch)
            .append_pair("after", &cursor.sequence.to_string());
    }
    let mut subscriptions = state
        .subscriptions
        .lock()
        .map_err(|_| "Event stream storage is busy".to_owned())?;
    if let Some(previous) = subscriptions.remove(&profile_id) {
        previous.abort();
    }
    let event_profile_id = profile_id.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            client.get(url).bearer_auth(token).send(),
        )
        .await
        .map_err(|_| "Event stream connection timed out".to_owned())
        .and_then(|response| response.map_err(|error| error.to_string()));
        let response = match response {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                let _ = app.emit("remote-disconnect", serde_json::json!({ "profileId": event_profile_id, "message": format!("Event stream rejected with {}", response.status()) }));
                return;
            }
            Err(error) => {
                let _ = app.emit(
                    "remote-disconnect",
                    serde_json::json!({ "profileId": event_profile_id, "message": error.to_string() }),
                );
                return;
            }
        };
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let frames = match decoder.push(&bytes) {
                        Ok(frames) => frames,
                        Err(error) => {
                            let _ = app.emit("remote-disconnect", serde_json::json!({ "profileId": event_profile_id, "message": error.to_string() }));
                            return;
                        }
                    };
                    for frame in frames {
                        match parse_remote_sse(&frame.event, &frame.data) {
                            Ok(Some(ParsedRemoteSse::Replay(replay))) => {
                                let _ = app.emit("remote-replay", replay);
                            }
                            Ok(Some(ParsedRemoteSse::Event(event))) => {
                                let _ = app.emit("remote-event", event);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                let _ = app.emit("remote-disconnect", serde_json::json!({ "profileId": event_profile_id, "message": error }));
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = app.emit("remote-disconnect", serde_json::json!({ "profileId": event_profile_id, "message": error.to_string() }));
                    return;
                }
            }
        }
        let _ = app.emit("remote-disconnect", serde_json::json!({ "profileId": event_profile_id, "message": "Event stream ended. Refresh the snapshot to reconnect." }));
    });
    subscriptions.insert(profile_id, handle);
    Ok(())
}

#[tauri::command]
fn remote_unsubscribe(state: State<'_, MobileState>, profile_id: String) {
    if let Ok(mut subscriptions) = state.subscriptions.lock() {
        if let Some(handle) = subscriptions.remove(&profile_id) {
            handle.abort();
        }
    }
}

#[tauri::command]
fn remote_cache_get(app: AppHandle, key: String) -> Result<Option<Value>, String> {
    let path = cache_dir(&app)?.join(format!("{}.json", hex(&Sha256::digest(key.as_bytes()))));
    if !path.exists() {
        return Ok(None);
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn remote_cache_put(app: AppHandle, key: String, value: Value) -> Result<(), String> {
    let path = cache_dir(&app)?.join(format!("{}.json", hex(&Sha256::digest(key.as_bytes()))));
    write_private_json(&path, &value)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(MobileState::default())
        .invoke_handler(tauri::generate_handler![
            remote_pair,
            remote_profiles,
            remote_forget,
            remote_request,
            remote_snapshot,
            remote_subscribe,
            remote_unsubscribe,
            remote_cache_get,
            remote_cache_put
        ])
        .setup(|app| {
            #[cfg(target_os = "ios")]
            app.handle().plugin(tauri_plugin_barcode_scanner::init())?;
            let _ = app_data(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Brr Mobile failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_normalization_ignores_colons_and_sha_prefix() {
        assert_eq!(normalize_fingerprint("SHA256:aa:bb"), "aabb");
    }

    #[test]
    fn non_https_and_credentials_in_endpoint_are_rejected() {
        assert!(endpoint_url("http://node.local", "/v1/pair").is_err());
        assert!(endpoint_url("https://user:pass@node.local", "/v1/pair").is_err());
        assert!(endpoint_url("https://node.local/prefix?token=leak", "/v1/pair").is_err());
        assert!(endpoint_url("https://node.local/ambiguous-prefix", "/v1/pair").is_err());
        assert!(endpoint_url("https://node.local/#credential", "/v1/pair").is_err());
        assert!(endpoint_url("https://node.local", "/v1/pair").is_ok());
    }

    #[test]
    fn pairing_rejects_a_certificate_pin_that_does_not_match_the_supplied_ca() {
        let certificate = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----";
        assert!(verify_certificate_pin(certificate, &"0".repeat(64)).is_err());
        assert!(pinned_client(certificate).is_err());
    }

    #[test]
    fn cache_file_keys_are_content_opaque() {
        assert_ne!(
            hex(&Sha256::digest(b"snapshot:a")),
            hex(&Sha256::digest(b"snapshot:b"))
        );
        assert!(!hex(&Sha256::digest(b"../profiles.json")).contains('/'));
    }

    #[test]
    fn parses_the_server_remote_event_and_snapshot_required_sse_names() {
        let event = r#"{"nodeId":"worker","epoch":"primary-epoch-1","sequence":7,"ownerEpoch":"worker-epoch-2","message":{"method":"item/agentMessage/delta","params":{"delta":"hello"}}}"#;
        assert!(matches!(
            parse_remote_sse("message", event),
            Ok(Some(ParsedRemoteSse::Event(_)))
        ));

        let replay =
            r#"{"epoch":"primary-epoch-1","latest":8,"requiresSnapshot":true,"events":[]}"#;
        match parse_remote_sse("snapshot_required", replay) {
            Ok(Some(ParsedRemoteSse::Replay(value))) => {
                assert_eq!(value.latest, 8);
                assert!(value.requires_snapshot);
            }
            _ => panic!("server snapshot_required frame must be forwarded as remote-replay"),
        }
    }

    #[test]
    fn malformed_known_sse_frames_fail_and_unknown_names_remain_ignorable() {
        assert!(parse_remote_sse("message", "not-json").is_err());
        assert!(matches!(
            parse_remote_sse("extension", "anything"),
            Ok(None)
        ));
    }
}
