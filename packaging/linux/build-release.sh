#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
OUTPUT_DIR="${1:-$ROOT/.cache/releases/linux}"
VERSION="${2:-$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$ROOT/package.json")}"
CONTAINER_NAME="brr-linux-release-$(date +%s)-$$"

if [[ ! "$VERSION" =~ ^[0-9][A-Za-z0-9.+~:-]*$ ]]; then
  echo "Invalid Debian package version: $VERSION" >&2
  exit 2
fi
command -v node >/dev/null || { echo "node is required" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }
command -v docker >/dev/null || { echo "docker is required" >&2; exit 2; }
BUILDER_RECIPE_SHA256="$(python3 - "$ROOT/packaging/linux/Dockerfile.build" <<'PY'
import hashlib, pathlib, sys
print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)"
BUILDER_RECIPE_SHA="${BUILDER_RECIPE_SHA256:0:12}"
BUILDER_IMAGE="brr-linux-builder:bookworm-$BUILDER_RECIPE_SHA"

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd -P)"
LOCK_DIR="$OUTPUT_DIR/.build-lock"
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
  echo "Another Linux release build owns $LOCK_DIR" >&2
  exit 1
fi

cleanup() {
  docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  if [[ -n "$STAGING_DIR" ]]; then rm -rf "$STAGING_DIR"; fi
  if [[ -n "$SOURCE_SCRATCH_DIR" ]]; then rm -rf "$SOURCE_SCRATCH_DIR"; fi
  rmdir "$LOCK_DIR" 2>/dev/null || true
}
STAGING_DIR=
SOURCE_SCRATCH_DIR=
trap cleanup EXIT INT TERM

STAGING_DIR="$(mktemp -d "$OUTPUT_DIR/.release-candidate.XXXXXX")"
mkdir -p "$ROOT/.cache/releases"
SOURCE_SCRATCH_DIR="$(mktemp -d "$ROOT/.cache/releases/.linux-release.XXXXXX")"
SOURCE_ARCHIVE="$STAGING_DIR/brr-source-$VERSION.tar.gz"
SOURCE_ARCHIVE_BUILD="$SOURCE_SCRATCH_DIR/brr-source-$VERSION.tar.gz"
node "$ROOT/scripts/prepare-source-release.mjs" "$SOURCE_ARCHIVE_BUILD"
cp "$SOURCE_ARCHIVE_BUILD" "$SOURCE_ARCHIVE"
SOURCE_SHA="$(python3 - "$SOURCE_ARCHIVE" <<'PY'
import hashlib, pathlib, sys
path = pathlib.Path(sys.argv[1])
digest = hashlib.sha256(path.read_bytes()).hexdigest()
path.with_suffix(path.suffix + '.sha256').write_text(f'{digest}  {path.name}\n')
print(digest)
PY
)"
printf 'Source archive SHA-256: %s\n' "$SOURCE_SHA"

# The Docker context contains only the pinned builder recipe. The source is
# copied into a fresh container at /work; the active /src build workspace is
# never mounted, reused, or modified.
if ! docker image inspect "$BUILDER_IMAGE" >/dev/null 2>&1; then
  docker build --platform linux/amd64 \
    --file "$ROOT/packaging/linux/Dockerfile.build" \
    --tag "$BUILDER_IMAGE" "$ROOT/packaging/linux"
fi

SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || echo 0)}"
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be an integer Unix timestamp" >&2
  exit 2
fi

docker create --platform linux/amd64 --name "$CONTAINER_NAME" \
  --workdir / \
  --env CARGO_BUILD_TARGET=x86_64-unknown-linux-gnu \
  --env CARGO_INCREMENTAL=0 \
  --env CARGO_PROFILE_DEV_DEBUG=0 \
  --env CARGO_PROFILE_RELEASE_DEBUG=0 \
  --env BRR_DEB_VERSION="$VERSION" \
  --env SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$BUILDER_IMAGE" sh -euc '
    mkdir -p /work/source /work/output
    tar -xzf /tmp/brr-source.tar.gz -C /work/source
    cd /work/source
    node scripts/build-runtime.mjs --release
    node scripts/build-node.mjs --release
    python3 packaging/build-deb.py \
      --binaries src-tauri/binaries \
      --browser src-tauri/binaries/browser \
      --version "$BRR_DEB_VERSION" \
      --output "/work/output/brr-node_${BRR_DEB_VERSION}_amd64.deb"
  ' >/dev/null
docker cp "$SOURCE_ARCHIVE" "$CONTAINER_NAME:/tmp/brr-source.tar.gz"
docker start --attach "$CONTAINER_NAME"
docker cp "$CONTAINER_NAME:/work/output/." "$STAGING_DIR/"
DEB_PATH="$STAGING_DIR/brr-node_${VERSION}_amd64.deb"
DEB_SIDECAR="$DEB_PATH.sha256"
[[ -f "$DEB_PATH" ]]
[[ -f "$DEB_SIDECAR" ]]
python3 - "$STAGING_DIR" "$VERSION" "$SOURCE_SHA" "$BUILDER_IMAGE" "$BUILDER_RECIPE_SHA256" "$SOURCE_DATE_EPOCH" <<'PY'
import hashlib, json, pathlib, re, sys, tarfile

output, version, initial_source_sha, image, recipe_sha, epoch = sys.argv[1:]
output = pathlib.Path(output)
source_name = f'brr-source-{version}.tar.gz'
package_name = f'brr-node_{version}_amd64.deb'

def verify_sidecar(name, expected_sha=None):
    artifact = output / name
    sidecar = output / f'{name}.sha256'
    lines = sidecar.read_text(encoding='utf-8').splitlines()
    if len(lines) != 1:
        raise SystemExit(f'Invalid staged checksum sidecar: {sidecar.name}')
    fields = lines[0].split()
    if len(fields) != 2 or fields[1] != name or not re.fullmatch(r'[0-9a-fA-F]{64}', fields[0]):
        raise SystemExit(f'Invalid staged checksum sidecar: {sidecar.name}')
    digest = hashlib.sha256()
    with artifact.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    actual = digest.hexdigest()
    if actual.lower() != fields[0].lower():
        label = 'Debian package' if name == package_name else 'Source archive'
        raise SystemExit(f'{label} checksum mismatch')
    if expected_sha is not None and actual != expected_sha:
        raise SystemExit('Source archive checksum changed between initial hashing and verification')
    return actual

source_sha = verify_sidecar(source_name, initial_source_sha)
package_sha = verify_sidecar(package_name)
with tarfile.open(output / source_name, 'r:gz') as archive:
    pins = [member for member in archive.getmembers()
            if member.name == 'packaging/browser-artifacts.json']
    if len(pins) != 1 or not pins[0].isfile():
        raise SystemExit('Source archive must contain one regular browser-artifacts.json pin file')
    pin_stream = archive.extractfile(pins[0])
    if pin_stream is None:
        raise SystemExit('Source archive browser-artifacts.json pin could not be read')
    with pin_stream:
        pin = json.load(pin_stream)
linux = next((asset for asset in pin['assets'] if asset['platform'] == 'linux64'), None)
if linux is None:
    raise SystemExit('Source archive browser pins omit linux64')

manifest = {
    'architecture': 'amd64',
    'target': 'x86_64-unknown-linux-gnu',
    'containerImage': image,
    'builderRecipeSha256': recipe_sha,
    'sourceArchive': source_name,
    'sourceArchiveSha256': source_sha,
    'sourceDateEpoch': int(epoch),
    'browserVersion': pin['version'],
    'browserArchiveSha256': linux['sha256'],
    'debianPackage': package_name,
    'debianPackageSha256': package_sha,
}
manifest_path = output / 'brr-linux-build.json'
manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
PY

# The manifest marks a complete release tuple. Remove any prior marker before
# replacing payloads and publish the new marker only after they all move.
OUTPUT_MANIFEST="$OUTPUT_DIR/brr-linux-build.json"
rm -f "$OUTPUT_MANIFEST"
mv -f "$SOURCE_ARCHIVE" "$OUTPUT_DIR/brr-source-$VERSION.tar.gz"
mv -f "$SOURCE_ARCHIVE.sha256" "$OUTPUT_DIR/brr-source-$VERSION.tar.gz.sha256"
mv -f "$DEB_PATH" "$OUTPUT_DIR/brr-node_${VERSION}_amd64.deb"
mv -f "$DEB_SIDECAR" "$OUTPUT_DIR/brr-node_${VERSION}_amd64.deb.sha256"
mv -f "$STAGING_DIR/brr-linux-build.json" "$OUTPUT_MANIFEST"
echo "Linux release artifacts written to $OUTPUT_DIR"
