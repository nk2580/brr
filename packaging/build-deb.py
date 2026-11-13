#!/usr/bin/env python3
"""Package source-built Brr service/runtime and its pinned browser as an amd64 .deb."""
import argparse
import hashlib
import json
import pathlib
import re
import shutil
import stat
import struct
import subprocess
import tempfile

BINARIES = ['brrd', 'brrctl', 'brr-runtime', 'codex-code-mode-host']


def amd64_elf(path):
    with path.open('rb') as source:
        header = source.read(64)
    if len(header) < 64 or header[:6] != b'\x7fELF\x02\x01' or struct.unpack_from('<H', header, 18)[0] != 62:
        raise ValueError(str(path) + ' must be a source-built Linux amd64 ELF binary')


def verify_pinned_browser(browser, root, browser_pin=None):
    """Verify every staged browser byte against the source-controlled ZIP pin."""
    browser, root = pathlib.Path(browser), pathlib.Path(root)
    if browser_pin is None:
        browser_pin = json.loads((root / 'packaging/browser-artifacts.json').read_text())
    asset = next((item for item in browser_pin['assets'] if item.get('platform') == 'linux64'), None)
    if asset is None or not isinstance(asset.get('files'), dict):
        raise ValueError('Source pin does not contain the Linux browser file manifest')
    expected_manifest = {'version': browser_pin['version'], **asset}
    manifest_path = browser / 'brr-browser-manifest.json'
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError('Managed browser manifest is missing or invalid')
    try:
        generated_manifest = json.loads(manifest_path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError('Managed browser manifest is missing or invalid') from error
    if generated_manifest != expected_manifest:
        raise ValueError('browser manifest does not match pinned artifact')

    actual_files = {}
    for item in browser.rglob('*'):
        relative = item.relative_to(browser).as_posix()
        if item.is_symlink():
            raise ValueError('Managed browser staging must not contain symlinks')
        mode = item.lstat().st_mode
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode):
            raise ValueError('Managed browser staging must contain only regular files')
        if relative == 'brr-browser-manifest.json':
            continue
        actual_files[relative] = hashlib.sha256(item.read_bytes()).hexdigest()

    expected_files = asset['files']
    if set(actual_files) != set(expected_files):
        missing = sorted(set(expected_files) - set(actual_files))
        extra = sorted(set(actual_files) - set(expected_files))
        raise ValueError(f'pinned browser file set mismatch (missing={missing}, extra={extra})')
    for relative, expected_hash in expected_files.items():
        if actual_files[relative] != expected_hash:
            raise ValueError('pinned browser file mismatch: ' + relative)


def stage(binaries, browser, destination, version, root, *, browser_pin=None):
    binaries, browser, destination, root = map(pathlib.Path, [binaries, browser, destination, root])
    if not re.fullmatch(r'[0-9][A-Za-z0-9.+~:-]*', version):
        raise ValueError('Invalid Debian package version')
    for name in BINARIES:
        amd64_elf(binaries / name)
    verify_pinned_browser(browser, root, browser_pin)
    amd64_elf(browser / 'chrome-headless-shell')
    if not (browser / 'LICENSE.headless_shell').is_file():
        raise ValueError('Stage the checksum-pinned managed browser with its license first')
    if destination.exists():
        raise ValueError('Package staging directory must be new')
    library = destination / 'usr/lib/brr'
    library.mkdir(parents=True)
    manifest = {'version': version, 'architecture':'amd64', 'files':{}}
    for name in BINARIES:
        shutil.copyfile(binaries / name, library / name)
        (library / name).chmod(0o755)
        manifest['files'][name] = hashlib.sha256((library / name).read_bytes()).hexdigest()
    # Older Bubblewrap releases must re-exec a real, visible helper path.
    (library / 'codex-linux-sandbox').symlink_to('brr-runtime')
    packaged_browser = library / 'browser'
    shutil.copytree(browser, packaged_browser)
    for item in [packaged_browser, *packaged_browser.rglob('*')]:
        if item.is_dir():
            item.chmod(0o755)
    commands = destination / 'usr/bin'
    commands.mkdir(parents=True)
    for name in ['brrd','brrctl']:
        (commands / name).symlink_to('../lib/brr/' + name)
    units = destination / 'usr/lib/systemd/user'
    units.mkdir(parents=True)
    shutil.copyfile(root / 'packaging/linux/brrd.service', units / 'brrd.service')
    docs = destination / 'usr/share/doc/brr-node'
    docs.mkdir(parents=True)
    for source, name in [('LICENSE','copyright'),('NOTICE','NOTICE'),('runtime/codex/LICENSE','Codex-LICENSE'),('runtime/codex/NOTICE','Codex-NOTICE'),('runtime/UPSTREAM.md','runtime-upstream.md'),('vendor/worktrunk/LICENSE','WorkTrunk-LICENSE'),('vendor/WORKTRUNK.md','worktrunk-upstream.md')]:
        shutil.copyfile(root / source, docs / name)
    shared = destination / 'usr/share/brr'
    shared.mkdir(parents=True)
    shutil.copyfile(root / 'packaging/linux/brr-userns.apparmor', shared / 'brr-userns.apparmor')
    (shared / 'build-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    control = destination / 'DEBIAN'
    control.mkdir()
    dependencies = ['bubblewrap (>= 0.8.0)','ca-certificates','git','libc6 (>= 2.36)','libgcc-s1','libstdc++6','libcap2','libssl3t64 | libssl3']
    if (browser / 'deb.deps').exists():
        dependencies += (browser / 'deb.deps').read_text().splitlines()
    (control / 'control').write_text(f'Package: brr-node\nVersion: {version}\nSection: utils\nPriority: optional\nArchitecture: amd64\nMaintainer: Brr contributors <brr@users.noreply.github.com>\nHomepage: https://github.com/nk2580/brr\nDepends: '+', '.join(dict.fromkeys(dependencies))+'\nDescription: Personal fleet execution service for Brr\n Headless per-user sessions and agents with a private Git fleet and iPhone control.\n')
    for script in ['postinst', 'prerm']:
        target = control / script
        shutil.copyfile(root / f'packaging/linux/apparmor-{script}', target)
        target.chmod(0o755)
    # Personal state is owned by the user outside dpkg and survives removal/purge.
    return destination


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries',required=True,type=pathlib.Path)
    parser.add_argument('--browser',required=True,type=pathlib.Path)
    parser.add_argument('--version',required=True)
    parser.add_argument('--output',required=True,type=pathlib.Path)
    args=parser.parse_args()
    root=pathlib.Path(__file__).resolve().parent.parent
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='brr-deb-') as directory:
        staged=stage(args.binaries,args.browser,pathlib.Path(directory)/'package',args.version,root)
        subprocess.run(['dpkg-deb','--root-owner-group','--build',str(staged),str(args.output.resolve())],check=True)
    digest=hashlib.sha256(args.output.read_bytes()).hexdigest()
    args.output.with_suffix(args.output.suffix+'.sha256').write_text(digest+'  '+args.output.name+'\n')
    print(args.output)
