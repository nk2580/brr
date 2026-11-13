#!/usr/bin/env python3
"""Stage the exact checksum-pinned Chrome headless build, including its notices."""
import argparse
import hashlib
import json
import pathlib
import shutil
import stat
import subprocess
import tempfile
import zipfile


def prepare(manifest, platform, cache, destination, download=True):
    asset = next((item for item in manifest['assets'] if item['platform'] == platform), None)
    if asset is None:
        raise ValueError('Unsupported managed browser platform: ' + platform)
    cache, destination = pathlib.Path(cache), pathlib.Path(destination)
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / (platform + '.zip')
    def matches():
        return archive.is_file() and hashlib.sha256(archive.read_bytes()).hexdigest() == asset['sha256']
    if not matches():
        if not download:
            raise ValueError('Managed browser SHA-256 verification failed')
        temporary = cache / (platform + '.download')
        subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error', asset['url'], '--output', str(temporary)], check=True)
        if hashlib.sha256(temporary.read_bytes()).hexdigest() != asset['sha256']:
            temporary.unlink(missing_ok=True)
            raise ValueError('Managed browser SHA-256 verification failed')
        temporary.replace(archive)
    destination.parent.mkdir(parents=True, exist_ok=True)
    prefix = 'chrome-headless-shell-' + platform
    with tempfile.TemporaryDirectory(prefix='.brr-browser-', dir=destination.parent) as staging:
        staged = pathlib.Path(staging)
        with zipfile.ZipFile(archive) as bundle:
            for item in bundle.infolist():
                path = pathlib.PurePosixPath(item.filename)
                mode = item.external_attr >> 16
                if path.is_absolute() or '..' in path.parts or not path.parts or path.parts[0] != prefix or stat.S_ISLNK(mode):
                    raise ValueError('Unsafe managed browser archive member')
                relative = pathlib.Path(*path.parts[1:])
                target = staged / relative
                if item.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with bundle.open(item) as source, target.open('xb') as output:
                        shutil.copyfileobj(source, output)
                    target.chmod(0o755 if mode & 0o111 else 0o644)
        executable = staged / 'chrome-headless-shell'
        if not executable.is_file():
            raise ValueError('Managed browser executable is missing')
        executable.chmod(0o755)
        (staged / 'brr-browser-manifest.json').write_text(json.dumps({'version':manifest['version'], **asset}, indent=2) + '\n')
        # Only the explicit generated destination is replaced, after full validation.
        if destination.is_symlink():
            raise ValueError('Managed browser destination must not be a symlink')
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(staged, destination)
    return destination / 'chrome-headless-shell'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', required=True, choices=['linux64', 'mac-arm64'])
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--cache', type=pathlib.Path)
    arguments = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parent.parent
    manifest = json.loads((root / 'packaging/browser-artifacts.json').read_text())
    print(prepare(manifest, arguments.platform, arguments.cache or root / '.cache/browser-artifacts', arguments.output))
