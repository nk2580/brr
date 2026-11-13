#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 PACKAGE.deb [debian:12-slim|ubuntu:24.04]" >&2
  echo "Runs installation and service checks in a disposable privileged systemd container." >&2
  echo "Inside a disposable VM: $0 --inside-vm PACKAGE.deb ROOT_OWNED_QA_MARKER [PREVIOUS.deb]" >&2
}

NATIVE_VM=0
PREVIOUS_PACKAGE=""
if [[ "${1:-}" == "--inside-vm" ]]; then
  NATIVE_VM=1
  PACKAGE="${2:-}"
  QA_MARKER="${3:-}"
  PREVIOUS_PACKAGE="${4:-}"
  [[ "$#" -le 4 ]] || { echo "Too many arguments for native VM verification" >&2; exit 2; }
  [[ "$(id -u)" == 0 ]] || { echo "Native VM verification must run as root" >&2; exit 2; }
  [[ -f "$PACKAGE" ]] || { echo "Package not found: $PACKAGE" >&2; exit 2; }
  command -v systemd-detect-virt >/dev/null || { echo "systemd-detect-virt is required" >&2; exit 2; }
  if systemd-detect-virt --container >/dev/null 2>&1; then
    echo "Refusing native VM verification inside a container" >&2
    exit 2
  fi
  if ! systemd-detect-virt --vm >/dev/null 2>&1; then
    echo "Refusing native install checks outside a disposable VM" >&2
    exit 2
  fi
  [[ -n "$QA_MARKER" && -f "$QA_MARKER" && ! -L "$QA_MARKER" ]] || {
    echo "A root-owned 0600 disposable-VM QA marker is required" >&2
    exit 2
  }
  [[ "$(stat -c '%u:%a' -- "$QA_MARKER")" == "0:600" ]] || {
    echo "The disposable-VM QA marker must be owned by root with mode 0600" >&2
    exit 2
  }
  [[ "$(cat -- "$QA_MARKER")" == "BRR_DISPOSABLE_VM_INSTALL_QA_V1" ]] || {
    echo "The disposable-VM QA marker content did not match" >&2
    exit 2
  }
  if [[ -n "$PREVIOUS_PACKAGE" ]]; then
    [[ "$PREVIOUS_PACKAGE" == /* ]] || {
      echo "Previous package path must be absolute" >&2
      exit 2
    }
    [[ -f "$PREVIOUS_PACKAGE" && ! -L "$PREVIOUS_PACKAGE" ]] || {
      echo "Previous package must be a regular non-symlink file" >&2
      exit 2
    }
    [[ -f "$PACKAGE" && ! -L "$PACKAGE" ]] || {
      echo "Package must be a regular non-symlink file" >&2
      exit 2
    }
    PREVIOUS_NAME="$(dpkg-deb -f "$PREVIOUS_PACKAGE" Package)"
    PREVIOUS_VERSION="$(dpkg-deb -f "$PREVIOUS_PACKAGE" Version)"
    PREVIOUS_ARCH="$(dpkg-deb -f "$PREVIOUS_PACKAGE" Architecture)"
    PACKAGE_NAME="$(dpkg-deb -f "$PACKAGE" Package)"
    PACKAGE_VERSION="$(dpkg-deb -f "$PACKAGE" Version)"
    PACKAGE_ARCH="$(dpkg-deb -f "$PACKAGE" Architecture)"
    [[ "$PREVIOUS_NAME" == brr-node && "$PACKAGE_NAME" == brr-node ]] || {
      echo "Upgrade packages must both be named brr-node" >&2
      exit 2
    }
    [[ "$PREVIOUS_ARCH" == amd64 && "$PACKAGE_ARCH" == amd64 ]] || {
      echo "Upgrade packages must both have amd64 architecture" >&2
      exit 2
    }
    dpkg --compare-versions "$PREVIOUS_VERSION" lt "$PACKAGE_VERSION" || {
      echo "Previous package version must be older than the package under test" >&2
      exit 2
    }
  fi
fi

if [[ "${1:-}" == "--inside-container" || "$NATIVE_VM" == 1 ]]; then
  PACKAGE="${2:?missing package path}"
  [[ "$(id -u)" == 0 ]] || { echo "The inner verifier must run as root" >&2; exit 2; }
  if [[ "$NATIVE_VM" == 0 ]]; then
    [[ -f /.dockerenv || -f /run/.containerenv ]] || { echo "Refusing to install outside a container" >&2; exit 2; }
  else
    if dpkg-query -W -f='${binary:Package}' brr-node >/dev/null 2>&1; then
      echo "Refusing native verification when any brr-node package record exists" >&2
      exit 2
    fi
    if getent passwd brr-check >/dev/null 2>&1; then
      echo "Refusing native verification when the brr-check user already exists" >&2
      exit 2
    fi
    if getent group brr-check >/dev/null 2>&1; then
      echo "Refusing native verification when the brr-check group already exists" >&2
      exit 2
    fi
    for path in /home/brr-check /var/mail/brr-check /var/spool/mail/brr-check; do
      if [[ -e "$path" || -L "$path" ]]; then
        echo "Refusing native verification because a brr-check home/mail path already exists: $path" >&2
        exit 2
      fi
    done
    for path in \
      /usr/bin/brrd /usr/bin/brrctl /usr/lib/brr \
      /usr/lib/systemd/user/brrd.service /usr/share/brr \
      /usr/share/doc/brr-node /etc/apparmor.d/brr-node; do
      if [[ -e "$path" || -L "$path" ]]; then
        echo "Refusing native verification because a Brr package path already exists: $path" >&2
        exit 2
      fi
    done
  fi
  [[ -f "$PACKAGE" ]] || { echo "Package not found: $PACKAGE" >&2; exit 2; }

  . /etc/os-release
  case "$ID:$VERSION_ID" in
    debian:12|ubuntu:24.04) ;;
    *) echo "Expected Debian 12 or Ubuntu 24.04; found $ID $VERSION_ID" >&2; exit 2 ;;
  esac
  SYSTEM_STATE="$(systemctl is-system-running 2>/dev/null || true)"
  case "$SYSTEM_STATE" in
    running|degraded) ;;
    *) echo "Test environment must run systemd as PID 1 (state: ${SYSTEM_STATE:-unknown})" >&2; exit 2 ;;
  esac

  TEMP_ROOT="$(mktemp -d /var/tmp/brr-install-check.XXXXXX)"
  TEST_USER="brr-check"
  TEST_HOME=""
  TEST_DATA=""
  TEST_CONFIG=""
  TEST_XDG=""
  INVITATION="$TEMP_ROOT/pairing.json"
  CREATED_USER=0
  cleanup() {
    if [[ "$CREATED_USER" == 1 ]]; then
      uid="$(id -u "$TEST_USER" 2>/dev/null || true)"
      if [[ -n "$uid" ]]; then
        loginctl disable-linger "$TEST_USER" >/dev/null 2>&1 || true
        systemctl stop "user@$uid.service" >/dev/null 2>&1 || true
      fi
      userdel -r "$TEST_USER" >/dev/null 2>&1 || true
    fi
    rm -rf -- "$TEMP_ROOT"
  }
  trap cleanup EXIT INT TERM

  VERSION="$(dpkg-deb -f "$PACKAGE" Version)"
  if [[ -n "$PREVIOUS_PACKAGE" ]]; then
    OLD_PACKAGE="$PREVIOUS_PACKAGE"
    EXPECTED_OLD_VERSION="$PREVIOUS_VERSION"
  else
    # Keep the existing synthetic lower-version path for local release checks.
    STAGE="$TEMP_ROOT/old-package"
    mkdir "$STAGE"
    dpkg-deb --extract "$PACKAGE" "$STAGE"
    dpkg-deb --control "$PACKAGE" "$STAGE/DEBIAN"
    sed -i "s/^Version: .*/Version: ${VERSION}~smoke1/" "$STAGE/DEBIAN/control"
    OLD_PACKAGE="$TEMP_ROOT/brr-node-old.deb"
    dpkg-deb --build --root-owner-group "$STAGE" "$OLD_PACKAGE" >/dev/null
    EXPECTED_OLD_VERSION="${VERSION}~smoke1"
  fi

  apt-get update -qq
  apt-get install -y --no-install-recommends "$OLD_PACKAGE" >/dev/null
  [[ "$(dpkg-query -W -f='${Version}' brr-node)" == "$EXPECTED_OLD_VERSION" ]]

  useradd --create-home --shell /bin/bash "$TEST_USER"
  CREATED_USER=1
  TEST_HOME="$(getent passwd "$TEST_USER" | cut -d: -f6)"
  TEST_CONFIG="$TEST_HOME/.config"
  TEST_XDG="$TEST_HOME/.local/share"
  TEST_DATA="$TEST_HOME/brr-data"
  TEST_UID="$(id -u "$TEST_USER")"
  runuser -u "$TEST_USER" -- mkdir -p "$TEST_CONFIG" "$TEST_XDG"
  install -d -o "$TEST_USER" -g "$TEST_USER" -m 0700 "$TEST_DATA"
  printf 'keep-this-test-only-state\n' > "$TEST_DATA/preserve-me.txt"
  chown "$TEST_USER:$TEST_USER" "$TEST_DATA/preserve-me.txt"
  chmod 0600 "$TEST_DATA/preserve-me.txt"

  apt-get install -y --no-install-recommends "$PACKAGE" >/dev/null
  [[ "$(dpkg-query -W -f='${Version}' brr-node)" == "$VERSION" ]]
  [[ "$(cat "$TEST_DATA/preserve-me.txt")" == keep-this-test-only-state ]]
  verify_amd64_elf() {
    python3 - "$@" <<'PY'
import pathlib, sys

for name in sys.argv[1:]:
    path = pathlib.Path(name)
    with path.open('rb') as stream:
        header = stream.read(64)
    if (len(header) != 64 or header[:4] != b'\x7fELF' or
            header[4] != 2 or header[5] != 1 or header[6] != 1 or
            int.from_bytes(header[18:20], 'little') != 62):
        raise SystemExit(f'Unexpected executable architecture or ELF header: {path}')
PY
  }
  for binary in brrd brrctl brr-runtime codex-code-mode-host browser/chrome-headless-shell; do
    path="/usr/lib/brr/$binary"
    [[ -x "$path" ]] || { echo "Missing executable: $path" >&2; exit 1; }
  done
  verify_amd64_elf /usr/lib/brr/brrd /usr/lib/brr/brrctl /usr/lib/brr/brr-runtime \
    /usr/lib/brr/codex-code-mode-host /usr/lib/brr/browser/chrome-headless-shell
  [[ "$(readlink /usr/bin/brrd)" == ../lib/brr/brrd ]]
  [[ "$(readlink /usr/bin/brrctl)" == ../lib/brr/brrctl ]]
  [[ "$(readlink /usr/lib/brr/codex-linux-sandbox)" == brr-runtime ]]
  [[ -f /usr/lib/systemd/user/brrd.service ]]
  BWRAP_HELP="$(bwrap --help 2>&1 || true)"
  [[ "$BWRAP_HELP" == *--as-pid-1* ]] || { echo "Installed bubblewrap lacks --as-pid-1" >&2; exit 1; }
  [[ "$BWRAP_HELP" == *--perms* ]] || { echo "Installed bubblewrap lacks --perms" >&2; exit 1; }

  loginctl enable-linger "$TEST_USER"
  systemctl start "user@$TEST_UID.service"
  for _ in $(seq 1 30); do
    [[ -S "/run/user/$TEST_UID/systemd/private" ]] && break
    sleep 1
  done
  [[ -S "/run/user/$TEST_UID/systemd/private" ]] || {
    echo "User systemd private socket did not start" >&2
    exit 1
  }

  as_user() {
    runuser -u "$TEST_USER" -- env \
      -u DBUS_SESSION_BUS_ADDRESS \
      XDG_CONFIG_HOME="$TEST_CONFIG" XDG_DATA_HOME="$TEST_XDG" \
      XDG_RUNTIME_DIR="/run/user/$TEST_UID" \
      "$@"
  }
  INSTALLED="$(as_user /usr/bin/brrctl --data-dir "$TEST_DATA" service install --start)"
  DEFINITION="${INSTALLED#Installed }"
  [[ "$DEFINITION" == "$TEST_CONFIG/systemd/user/brrd-"*.service && -f "$DEFINITION" ]]

  for _ in $(seq 1 30); do
    if as_user /usr/bin/brrctl --data-dir "$TEST_DATA" status > "$TEMP_ROOT/status.json" 2>/dev/null; then
      break
    fi
    sleep 1
  done
  DAEMON_PID="$(python3 - "$TEMP_ROOT/status.json" "$TEST_UID" <<'PY'
import json, os, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
pid = int(data['pid'])
if os.stat(f'/proc/{pid}').st_uid != int(sys.argv[2]):
    raise SystemExit('brrd did not run as the nonroot test account')
print(pid)
PY
  )"
  for _ in $(seq 1 60); do
    if as_user /usr/bin/brrctl --data-dir "$TEST_DATA" status > "$TEMP_ROOT/status.json" 2>/dev/null \
      && python3 - "$TEMP_ROOT/status.json" <<'PY'
import json, pathlib, sys
try:
    status = json.loads(pathlib.Path(sys.argv[1]).read_text())
except (OSError, json.JSONDecodeError):
    raise SystemExit(1)
raise SystemExit(0 if status.get('runtimeConnected') is True else 1)
PY
    then
      break
    fi
    sleep 1
  done
  python3 - "$TEMP_ROOT/status.json" <<'PY'
import json, pathlib, sys
status = json.loads(pathlib.Path(sys.argv[1]).read_text())
if status.get('runtimeConnected') is not True:
    raise SystemExit('The packaged owned runtime did not become ready')
if status.get('runtimeError'):
    raise SystemExit(f"The owned runtime reported an error: {status['runtimeError']}")
PY
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" account status > "$TEMP_ROOT/account.json"
  python3 - "$TEMP_ROOT/account.json" <<'PY'
import json, pathlib, sys
account = json.loads(pathlib.Path(sys.argv[1]).read_text())
if 'account' not in account or account['account'] is not None:
    raise SystemExit('Install smoke requires an explicitly logged-out ChatGPT account state')
PY
  SANDBOX_ROOT="$TEST_HOME/sandbox-service"
  SANDBOX_WORKSPACE="$SANDBOX_ROOT/workspace"
  SANDBOX_ORIGINALS="$SANDBOX_ROOT/selected"
  SANDBOX_PRIVATE="$SANDBOX_ROOT/private"
  mkdir -p -m 0700 "$SANDBOX_WORKSPACE" "$SANDBOX_ORIGINALS" "$SANDBOX_PRIVATE"
  printf 'selected-original-readable\n' > "$SANDBOX_ORIGINALS/allowed.txt"
  printf 'private-sibling-secret\n' > "$SANDBOX_PRIVATE/secret.txt"
  chown -R "$TEST_USER:$TEST_USER" "$SANDBOX_ROOT"
  chmod 0600 "$SANDBOX_ORIGINALS/allowed.txt" "$SANDBOX_PRIVATE/secret.txt"
  as_user test -r "$SANDBOX_PRIVATE/secret.txt"
  SANDBOX_PROFILE="$(python3 - "$SANDBOX_ROOT" "$SANDBOX_WORKSPACE" "$SANDBOX_ORIGINALS" <<'PY'
import json, sys
service, workspace, selected = sys.argv[1:]
def path_rule(path, access):
    return {'path': {'type': 'path', 'path': path}, 'access': access}
print(json.dumps({
    'type': 'managed',
    'file_system': {
        'type': 'restricted',
        'entries': [
            {'path': {'type': 'special', 'value': {'kind': 'minimal'}}, 'access': 'read'},
            path_rule('/usr/lib/brr', 'read'),
            path_rule(service, 'deny'),
            path_rule(selected, 'read'),
            path_rule(workspace, 'write'),
        ],
    },
    'network': 'restricted',
}))
PY
  )"
  SANDBOX_COMMAND='test "$(cat "$1")" = selected-original-readable; if cat "$2" >/dev/null 2>&1; then echo "private sibling was readable" >&2; exit 91; fi; if (printf wrong > "$1") 2>/dev/null; then echo "original was writable" >&2; exit 92; fi; printf sandbox-write > "$3"; echo sandbox-ok'
  as_user /usr/lib/brr/codex-linux-sandbox --sandbox-policy-cwd "$SANDBOX_WORKSPACE" \
    --permission-profile "$SANDBOX_PROFILE" --no-proc -- /bin/sh -eu -c "$SANDBOX_COMMAND" \
    sandbox-check "$SANDBOX_ORIGINALS/allowed.txt" "$SANDBOX_PRIVATE/secret.txt" "$SANDBOX_WORKSPACE/probe.txt" \
    > "$TEMP_ROOT/sandbox-check.txt"
  grep -qx 'sandbox-ok' "$TEMP_ROOT/sandbox-check.txt" || {
    echo "Owned runtime sandbox did not verify workspace access and sibling denial" >&2
    cat "$TEMP_ROOT/sandbox-check.txt" >&2
    exit 1
  }
  [[ "$(cat "$SANDBOX_WORKSPACE/probe.txt")" == sandbox-write ]]
  [[ "$(cat "$SANDBOX_ORIGINALS/allowed.txt")" == selected-original-readable ]]
  if as_user /usr/bin/brrctl --data-dir "$TEST_DATA" request desktop engine_request \
    --params '{"method":"turn/start","params":{"threadId":"signed-out-check","input":[{"type":"text","text":"This verification must never execute inference."}]}}' \
    > "$TEMP_ROOT/logged-out-inference.txt" 2>&1; then
    echo "Logged-out runtime unexpectedly accepted an inference request" >&2
    exit 1
  fi
  grep -Fqi 'Sign in with ChatGPT' "$TEMP_ROOT/logged-out-inference.txt" || {
    echo "Inference was rejected for a reason other than the logged-out account boundary" >&2
    cat "$TEMP_ROOT/logged-out-inference.txt" >&2
    exit 1
  }
  [[ "$(stat -c %a "$TEST_DATA/node/brrd.sock")" == 600 ]]
  [[ "$(stat -c %u "$TEST_DATA/node/brrd.sock")" == "$TEST_UID" ]]

  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" request desktop browser_request \
    --params '{"threadId":"packaged-browser-smoke","args":{"action":"open","url":"about:blank"}}' \
    > "$TEMP_ROOT/browser-open.json"
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" request desktop browser_request \
    --params '{"threadId":"packaged-browser-smoke","args":{"action":"snapshot"}}' \
    > "$TEMP_ROOT/browser-snapshot.json"
  python3 - "$TEMP_ROOT/browser-open.json" "$TEMP_ROOT/browser-snapshot.json" <<'PY'
import json, pathlib, sys
opened, snapshot = (json.loads(pathlib.Path(path).read_text()) for path in sys.argv[1:])
if not opened.get('id') or opened.get('url') != 'about:blank':
    raise SystemExit('Packaged Chromium did not open the smoke page over local IPC')
if snapshot.get('url') != 'about:blank' or 'text' not in snapshot:
    raise SystemExit('Packaged Chromium snapshot failed over local IPC')
PY

  ATTACHMENT_OWNER="qa-unused-draft-attachment"
  ATTACHMENT_NAME="canonical-backup-check.txt"
  ATTACHMENT_TEXT="Brr canonical attachment backup fixture v1"
  ATTACHMENT_BASE64="QnJyIGNhbm9uaWNhbCBhdHRhY2htZW50IGJhY2t1cCBmaXh0dXJlIHYxCg=="
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" request chat attachments.importBytes \
    --params "{\"ownerId\":\"$ATTACHMENT_OWNER\",\"files\":[{\"name\":\"$ATTACHMENT_NAME\",\"mime\":\"text/plain\",\"data\":\"$ATTACHMENT_BASE64\"}]}" \
    > "$TEMP_ROOT/attachment-import.json"
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" request chat attachments.list \
    --params "{\"ownerId\":\"$ATTACHMENT_OWNER\"}" > "$TEMP_ROOT/attachment-list.json"
  ATTACHMENT_ID="$(python3 - "$TEMP_ROOT/attachment-list.json" <<'PY'
import json,sys
rows=json.load(open(sys.argv[1], encoding="utf-8"))
if not isinstance(rows,list) or len(rows)!=1 or not isinstance(rows[0].get("id"),str):
    raise SystemExit("Expected exactly one imported unused-draft attachment")
print(rows[0]["id"])
PY
)"
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" request chat attachments.preview \
    --params "{\"ownerId\":\"$ATTACHMENT_OWNER\",\"id\":\"$ATTACHMENT_ID\"}" \
    > "$TEMP_ROOT/attachment-preview.json"
  python3 - "$TEMP_ROOT/attachment-import.json" "$TEMP_ROOT/attachment-list.json" \
    "$TEMP_ROOT/attachment-preview.json" "$ATTACHMENT_ID" "$ATTACHMENT_NAME" \
    "$ATTACHMENT_TEXT" <<'PY'
import json, pathlib, sys
imported, listed, preview = (json.loads(pathlib.Path(path).read_text()) for path in sys.argv[1:4])
attachment_id, name, text = sys.argv[4:]
if not isinstance(imported, list) or len(imported) != 1 or imported[0].get('id') != attachment_id:
    raise SystemExit('Attachment import did not return exactly the listed attachment')
if len(listed) != 1 or listed[0].get('id') != attachment_id or listed[0].get('name') != name:
    raise SystemExit('Public attachment list did not preserve the imported draft metadata')
if preview.get('attachment', {}).get('id') != attachment_id or preview.get('preview') != text + '\n':
    raise SystemExit('Public attachment preview did not preserve the exact synthetic text bytes')
PY
  verify_attachment_store() {
    python3 - "$TEST_DATA" "$ATTACHMENT_ID" "$ATTACHMENT_OWNER" \
      "$ATTACHMENT_NAME" "$ATTACHMENT_TEXT" "$1" <<'PY'
import hashlib, pathlib, sqlite3, sys
data_dir = pathlib.Path(sys.argv[1])
attachment_id, owner, name, text, stage = sys.argv[2:]
expected = (text + '\n').encode()
database = data_dir / 'engine/agents/chat/chat.sqlite3'
connection = sqlite3.connect(f'file:{database}?mode=ro', uri=True)
try:
    rows = connection.execute(
        'SELECT a.blob_id,a.name,a.mime,a.size FROM chat_attachments a '
        'JOIN chat_attachment_grants g ON g.attachment_id=a.id '
        'WHERE a.id=? AND g.owner_id=?', (attachment_id, owner)
    ).fetchall()
finally:
    connection.close()
if len(rows) != 1:
    raise SystemExit(f'{stage}: canonical attachment registry row is missing or duplicated')
blob_id, found_name, mime, size = rows[0]
if (found_name, mime, size) != (name, 'text/plain', len(expected)):
    raise SystemExit(f'{stage}: canonical attachment metadata changed')
blob = data_dir / 'engine/agents/chat/blobs' / blob_id
if not blob.is_file() or blob.is_symlink() or blob.read_bytes() != expected:
    raise SystemExit(f'{stage}: canonical attachment blob bytes changed')
print(hashlib.sha256(expected).hexdigest())
PY
  }
  ATTACHMENT_SHA256="$(verify_attachment_store before-fleet-init)"

  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" fleet init > "$TEMP_ROOT/fleet-init.json"
  python3 - "$TEMP_ROOT/fleet-init.json" "$TEST_DATA" "$ATTACHMENT_ID" \
    "$ATTACHMENT_OWNER" "$ATTACHMENT_NAME" "$ATTACHMENT_TEXT" "$ATTACHMENT_SHA256" <<'PY'
import hashlib, json, pathlib, sqlite3, sys
init_path, data_path, attachment_id, owner, name, text, expected_sha = sys.argv[1:]
init = json.loads(pathlib.Path(init_path).read_text())
backup_root = pathlib.Path(data_path) / 'node/import-backups'
backup_dir = pathlib.Path(init.get('backupDirectory', ''))
if backup_dir.is_symlink() or not backup_dir.is_dir() or not backup_dir.resolve().is_relative_to(backup_root.resolve()):
    raise SystemExit('Fleet init did not return a private backup beneath node/import-backups')
manifest = json.loads((backup_dir / 'manifest.json').read_text())
databases = [item for item in manifest.get('databases', []) if item.get('name') == 'chat']
if len(databases) != 1 or databases[0].get('relativePath') != 'databases/chat.sqlite3':
    raise SystemExit('Fleet backup omitted the canonical chat database snapshot')
chat_path = backup_dir / databases[0]['relativePath']
if not chat_path.is_file() or chat_path.is_symlink():
    raise SystemExit('Fleet backup chat database snapshot is missing or unsafe')
chat_bytes = chat_path.read_bytes()
if len(chat_bytes) != databases[0].get('sizeBytes') or hashlib.sha256(chat_bytes).hexdigest() != databases[0].get('sha256'):
    raise SystemExit('Fleet backup chat database snapshot does not match its manifest')
connection = sqlite3.connect(f'file:{chat_path}?mode=ro', uri=True)
try:
    rows = connection.execute(
        'SELECT a.blob_id,a.name,a.mime,a.size FROM chat_attachments a '
        'JOIN chat_attachment_grants g ON g.attachment_id=a.id '
        'WHERE a.id=? AND g.owner_id=?', (attachment_id, owner)
    ).fetchall()
finally:
    connection.close()
if len(rows) != 1 or (rows[0][1], rows[0][2], rows[0][3]) != (name, 'text/plain', len((text+'\n').encode())):
    raise SystemExit('Canonical chat snapshot does not contain the imported draft attachment')
blob_id = rows[0][0]
relative = f'files/app-attachment-blob/{blob_id}'
files = [item for item in manifest.get('files', []) if item.get('relativePath') == relative]
if len(files) != 1 or files[0].get('classification') != 'app-attachment-blob':
    raise SystemExit('Fleet backup manifest omitted the exact app-attachment-blob entry')
expected = (text + '\n').encode()
blob_path = backup_dir / relative
blob_bytes = blob_path.read_bytes() if blob_path.is_file() and not blob_path.is_symlink() else None
digest = hashlib.sha256(expected).hexdigest()
if (blob_bytes != expected or files[0].get('sizeBytes') != len(expected)
        or files[0].get('sha256') != digest or digest != expected_sha):
    raise SystemExit('Fleet backup attachment bytes, size, or SHA256 did not match the imported file')
print(f'Verified canonical chat snapshot and attachment blob SHA256 {digest}')
PY
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" fleet status > "$TEMP_ROOT/fleet.json"
  (umask 077; as_user /usr/bin/brrctl --data-dir "$TEST_DATA" pair --endpoint https://localhost:8787 > "$INVITATION")
  python3 - "$INVITATION" <<'PY'
import hashlib, http.client, json, pathlib, ssl, sys
from urllib.parse import urlsplit

invite = json.loads(pathlib.Path(sys.argv[1]).read_text())
if not invite.get('code') or not invite.get('endpoint', '').startswith('https://localhost:'):
    raise SystemExit('local IPC pairing did not return a valid test invitation')
if not invite.get('certificatePem') or not invite.get('fingerprint') or not invite.get('nodeId'):
    raise SystemExit('local IPC invitation omitted TLS pin or node identity')
endpoint = urlsplit(invite['endpoint'])
context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
context.load_verify_locations(cadata=invite['certificatePem'])
connection = http.client.HTTPSConnection(endpoint.hostname, endpoint.port or 443, context=context, timeout=10)
body = json.dumps({'code': invite['code'], 'requestedRole': 'mobile', 'name': 'Disposable install verifier'})
try:
    ca_der = ssl.PEM_cert_to_DER_cert(invite['certificatePem'])
    expected = invite['fingerprint'].removeprefix('sha256:').replace(':', '').lower()
    if hashlib.sha256(ca_der).hexdigest() != expected:
        raise SystemExit('Invitation CA certificate did not match its SHA-256 fingerprint')
    connection.connect()
    if connection.sock is None or not connection.sock.getpeercert(binary_form=True):
        raise SystemExit('Pinned TLS pairing did not establish a verified server certificate')
    connection.request('POST', '/v1/pair', body=body, headers={'Content-Type': 'application/json'})
    response = connection.getresponse()
    if response.status != 200:
        raise SystemExit(f'Pinned TLS pairing redemption failed with HTTP {response.status}')
    credential = json.loads(response.read())
    if credential.get('nodeId') != invite['nodeId'] or credential.get('role') != 'mobile' or not credential.get('secret'):
        raise SystemExit('Pinned TLS redemption returned an unexpected device identity')
    connection.request('GET', '/v1/snapshot', headers={'Authorization': 'Bearer ' + credential['secret']})
    snapshot_response = connection.getresponse()
    if snapshot_response.status != 200:
        raise SystemExit(f'Authenticated snapshot request failed with HTTP {snapshot_response.status}')
    snapshot = json.loads(snapshot_response.read())
    if snapshot.get('nodeId') != invite['nodeId'] or snapshot.get('protocolVersion') != 1:
        raise SystemExit('Authenticated snapshot identity or protocol version did not match')
    if not isinstance(snapshot.get('cursor'), dict) or not isinstance(snapshot.get('nodes'), list):
        raise SystemExit('Authenticated snapshot omitted fleet cursor or node inventory')
finally:
    connection.close()
PY
  chmod 0600 "$INVITATION"

  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" service status >/dev/null
  as_user /usr/bin/brrctl --data-dir "$TEST_DATA" service uninstall
  if as_user /usr/bin/brrctl --data-dir "$TEST_DATA" service status \
    > "$TEMP_ROOT/uninstalled-service-status.txt" 2>&1; then
    echo "Service status unexpectedly succeeded after uninstall" >&2
    cat "$TEMP_ROOT/uninstalled-service-status.txt" >&2
    exit 1
  fi
  [[ ! -e "$DEFINITION" ]]
  [[ ! -e "$TEST_DATA/node/brrd.sock" ]]
  ! kill -0 "$DAEMON_PID" 2>/dev/null
  [[ "$(cat "$TEST_DATA/preserve-me.txt")" == keep-this-test-only-state ]]
  verify_attachment_store after-service-uninstall

  apt-get purge -y brr-node >/dev/null
  [[ ! -e /usr/lib/brr/brrd ]]
  [[ "$(cat "$TEST_DATA/preserve-me.txt")" == keep-this-test-only-state ]]
  verify_attachment_store after-package-purge
  echo "Install, upgrade, owned-runtime/logged-out inference, managed-browser IPC, canonical attachment backup, pinned-TLS pairing/snapshot, fleet/service, purge, and data-preservation checks passed on $PRETTY_NAME"
  exit 0
fi

PACKAGE="${1:-}"
DISTRO="${2:-debian:12-slim}"
if [[ -z "$PACKAGE" || ! -f "$PACKAGE" ]]; then
  usage
  exit 2
fi
case "$DISTRO" in debian:12-slim|ubuntu:24.04) ;; *) usage; exit 2 ;; esac
command -v docker >/dev/null || { echo "docker is required" >&2; exit 2; }
PACKAGE="$(cd "$(dirname "$PACKAGE")" && pwd -P)/$(basename "$PACKAGE")"
NAME="brr-install-check-$(date +%s)-$$"
IMAGE="brr-install-check:${DISTRO//[:.]/-}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

docker build --platform linux/amd64 --build-arg "BASE_IMAGE=$DISTRO" \
  --file "$ROOT/Dockerfile.verify" --tag "$IMAGE" "$ROOT"
docker run --detach --privileged --platform linux/amd64 --cgroupns private --name "$NAME" \
  --tmpfs /run --tmpfs /run/lock \
  "$IMAGE" >/dev/null
for _ in $(seq 1 30); do
  STATE="$(docker exec "$NAME" systemctl is-system-running 2>/dev/null || true)"
  if [[ "$STATE" == running || "$STATE" == degraded ]]; then break; fi
  sleep 1
done
STATE="$(docker exec "$NAME" systemctl is-system-running 2>/dev/null || true)"
[[ "$STATE" == running || "$STATE" == degraded ]] || {
  docker logs "$NAME" >&2 || true
  echo "Test systemd container did not become ready" >&2
  exit 1
}
docker cp "$PACKAGE" "$NAME:/tmp/brr-node.deb"
docker exec "$NAME" /usr/local/sbin/brr-verify-install --inside-container /tmp/brr-node.deb
