import hashlib, importlib.util, json, os, pathlib, struct, subprocess, tempfile, unittest


def make_stage(module, root, stage):
    binaries = root / 'binaries'
    binaries.mkdir()
    browser = root / 'browser'
    browser.mkdir()
    elf = bytearray(64)
    elf[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<H', elf, 18, 62)
    for name in ['brrd', 'brrctl', 'brr-runtime', 'codex-code-mode-host']:
        (binaries / name).write_bytes(elf)
    (browser / 'chrome-headless-shell').write_bytes(elf)
    (browser / 'LICENSE.headless_shell').write_text('fixture license')
    files = {name: hashlib.sha256((browser / name).read_bytes()).hexdigest() for name in ['chrome-headless-shell', 'LICENSE.headless_shell']}
    asset = {'platform': 'linux64', 'url': 'https://fixture.invalid/browser.zip', 'sha256': 'fixture-archive-sha', 'files': files}
    pin = {'version': 'fixture', 'assets': [asset]}
    (browser / 'brr-browser-manifest.json').write_text(json.dumps({'version': pin['version'], **asset}))
    project = pathlib.Path(__file__).resolve().parent.parent
    module.stage(binaries, browser, stage, '0.1.0-test', project, browser_pin=pin)

class DebPackageTest(unittest.TestCase):
    def test_stages_owned_amd64_binaries_and_user_service_preserving_data_on_removal(self):
        spec=importlib.util.spec_from_file_location('deb',pathlib.Path(__file__).with_name('build-deb.py'))
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root=pathlib.Path(directory);binaries=root/'binaries';binaries.mkdir();browser=root/'browser';browser.mkdir()
            elf=bytearray(64);elf[:6]=b'\x7fELF\x02\x01';struct.pack_into('<H',elf,18,62)
            for name in ['brrd','brrctl','brr-runtime','codex-code-mode-host']:(binaries/name).write_bytes(elf)
            (browser/'chrome-headless-shell').write_bytes(elf);(browser/'chrome-headless-shell').chmod(0o755)
            (browser/'LICENSE.headless_shell').write_text('fixture license');(browser/'LICENSE.headless_shell').chmod(0o640)
            nested=browser/'resources'/'locale';nested.mkdir(parents=True)
            (nested/'strings.pak').write_bytes(b'localized browser data');(nested/'strings.pak').chmod(0o640)
            (browser/'resources').chmod(0o700);nested.chmod(0o700);browser.chmod(0o700)
            browser_files=['chrome-headless-shell','LICENSE.headless_shell','resources/locale/strings.pak']
            pinned_modes={name:(browser/name).stat().st_mode & 0o777 for name in browser_files}
            pin={'version':'fixture','assets':[{'platform':'linux64','url':'https://fixture.invalid/browser.zip','sha256':'fixture-archive-sha','files':{name:hashlib.sha256((browser/name).read_bytes()).hexdigest() for name in browser_files}}]}
            (browser/'brr-browser-manifest.json').write_text(json.dumps({'version':pin['version'],**pin['assets'][0]}))
            stage=root/'stage';module.stage(binaries,browser,stage,'0.1.0-test',pathlib.Path(__file__).resolve().parent.parent,browser_pin=pin)
            control = (stage/'DEBIAN/control').read_text()
            self.assertIn('Architecture: amd64', control)
            self.assertIn('bubblewrap (>= 0.8.0)', control, 'runtime sandbox launch requires compatible system bubblewrap')
            self.assertEqual((stage/'usr/bin/brrctl').readlink(),pathlib.Path('../lib/brr/brrctl'))
            sandbox_alias=stage/'usr/lib/brr/codex-linux-sandbox'
            self.assertEqual(sandbox_alias.readlink(),pathlib.Path('brr-runtime'))
            self.assertEqual(sandbox_alias.resolve().read_bytes(),bytes(elf))
            packaged_browser=stage/'usr/lib/brr/browser'
            for directory in [packaged_browser,packaged_browser/'resources',packaged_browser/'resources/locale']:
                self.assertEqual(directory.stat().st_mode & 0o777,0o755)
            for name,mode in pinned_modes.items():
                self.assertEqual((packaged_browser/name).stat().st_mode & 0o777,mode)
                self.assertEqual((packaged_browser/name).read_bytes(),(browser/name).read_bytes())
            service=(stage/'usr/lib/systemd/user/brrd.service').read_text()
            self.assertIn('ExecStart=/usr/lib/brr/brrd',service);self.assertNotIn('User=root',service)
            self.assertFalse((stage/'DEBIAN/postrm').exists(),'package removal must not delete personal state')
            (binaries/'brrd').write_bytes(b'not an owned Linux build')
            with self.assertRaisesRegex(ValueError,'amd64 ELF'):module.stage(binaries,browser,root/'bad-stage','0.1.0-test',pathlib.Path(__file__).resolve().parent.parent,browser_pin=pin)

    def test_rejects_tampered_elf_and_stale_browser_manifest_before_staging(self):
        spec=importlib.util.spec_from_file_location('deb',pathlib.Path(__file__).with_name('build-deb.py'))
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root=pathlib.Path(directory);binaries=root/'binaries';binaries.mkdir();browser=root/'browser';browser.mkdir()
            elf=bytearray(64);elf[:6]=b'\x7fELF\x02\x01';struct.pack_into('<H',elf,18,62)
            for name in ['brrd','brrctl','brr-runtime','codex-code-mode-host']:(binaries/name).write_bytes(elf)
            (browser/'chrome-headless-shell').write_bytes(elf);(browser/'LICENSE.headless_shell').write_text('fixture license')
            files={name:hashlib.sha256((browser/name).read_bytes()).hexdigest() for name in ['chrome-headless-shell','LICENSE.headless_shell']}
            asset={'platform':'linux64','url':'https://fixture.invalid/browser.zip','sha256':'fixture-archive-sha','files':files}
            pin={'version':'fixture','assets':[asset]}
            browser_manifest=browser/'brr-browser-manifest.json'
            browser_manifest.write_text(json.dumps({'version':'fixture',**asset}))
            project=pathlib.Path(__file__).resolve().parent.parent
            (browser/'chrome-headless-shell').write_bytes(elf[:63]+bytes([elf[63]^1]))
            with self.assertRaisesRegex(ValueError,'pinned browser file mismatch'):
                module.stage(binaries,browser,root/'tampered-stage','0.1.0-test',project,browser_pin=pin)
            self.assertFalse((root/'tampered-stage').exists())
            (browser/'chrome-headless-shell').write_bytes(elf)
            browser_manifest.write_text(json.dumps({'version':'older',**asset}))
            with self.assertRaisesRegex(ValueError,'browser manifest does not match pinned artifact'):
                module.stage(binaries,browser,root/'stale-stage','0.1.0-test',project,browser_pin=pin)
            self.assertFalse((root/'stale-stage').exists())

    def test_apparmor_profile_is_boot_visible_only_on_ubuntu_and_unloads_on_remove(self):
        spec = importlib.util.spec_from_file_location('deb', pathlib.Path(__file__).with_name('build-deb.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            stage = root / 'stage'
            make_stage(module, root, stage)
            fake_root = root / 'ubuntu-root'
            (fake_root / 'etc/apparmor.d').mkdir(parents=True)
            (fake_root / 'etc/os-release').write_text('ID=ubuntu\nVERSION_ID="24.04"\n')
            (fake_root / 'sys/kernel/security/apparmor').mkdir(parents=True)
            (fake_root / 'sys/kernel/security/apparmor/.access').touch()
            source = fake_root / 'usr/share/brr'
            source.mkdir(parents=True)
            for item in (stage / 'usr/share/brr').iterdir():
                if item.is_file():
                    (source / item.name).write_bytes(item.read_bytes())
            parser_dir = root / 'bin'
            parser_dir.mkdir()
            parser_log = root / 'parser.log'
            parser = parser_dir / 'apparmor_parser'
            parser.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$APPARMOR_LOG"\n')
            parser.chmod(0o755)
            env = {**os.environ, 'DPKG_ROOT': str(fake_root), 'APPARMOR_LOG': str(parser_log), 'PATH': f'{parser_dir}:{os.environ["PATH"]}'}
            postinst = stage / 'DEBIAN/postinst'
            prerm = stage / 'DEBIAN/prerm'
            self.assertTrue(prerm.is_file(), 'package must install a removal hook for live-profile cleanup')
            subprocess.run([postinst, 'configure'], env=env, check=True)
            profile = fake_root / 'etc/apparmor.d/brr-node'
            self.assertTrue(profile.is_symlink(), 'Ubuntu policy should be visible to the AppArmor boot loader')
            self.assertEqual(os.readlink(profile), '../../usr/share/brr/brr-userns.apparmor')
            self.assertIn('-r', parser_log.read_text())
            subprocess.run([postinst, 'configure'], env=env, check=True)
            subprocess.run([prerm, 'remove'], env=env, check=True)
            self.assertFalse(profile.exists())
            self.assertIn('-R', parser_log.read_text())

            debian_root = root / 'debian-root'
            (debian_root / 'etc/apparmor.d').mkdir(parents=True)
            (debian_root / 'etc/os-release').write_text('ID=debian\nVERSION_ID="12"\n')
            debian_share = debian_root / 'usr/share/brr'
            debian_share.mkdir(parents=True)
            for item in (stage / 'usr/share/brr').iterdir():
                if item.is_file():
                    (debian_share / item.name).write_bytes(item.read_bytes())
            debian_env = {**env, 'DPKG_ROOT': str(debian_root)}
            parser_calls_before_debian = parser_log.read_text()
            subprocess.run([postinst, 'configure'], env=debian_env, check=True)
            subprocess.run([prerm, 'remove'], env=debian_env, check=True)
            self.assertEqual(list((debian_root / 'etc/apparmor.d').iterdir()), [], 'Debian must not receive the Ubuntu-only policy')
            self.assertEqual(parser_log.read_text(), parser_calls_before_debian, 'Debian must not invoke the AppArmor parser')

    def test_apparmor_lifecycle_preserves_admin_owned_policy_path(self):
        spec = importlib.util.spec_from_file_location('deb', pathlib.Path(__file__).with_name('build-deb.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            stage = root / 'stage'
            make_stage(module, root, stage)
            fake_root = root / 'ubuntu-root'
            apparmor = fake_root / 'etc/apparmor.d'
            apparmor.mkdir(parents=True)
            (fake_root / 'etc/os-release').write_text('ID=ubuntu\nVERSION_ID="24.04"\n')
            share = fake_root / 'usr/share/brr'
            share.mkdir(parents=True)
            for item in (stage / 'usr/share/brr').iterdir():
                if item.is_file():
                    (share / item.name).write_bytes(item.read_bytes())
            admin_policy = apparmor / 'brr-node'
            admin_policy.write_text('administrator policy\n')
            env = {**os.environ, 'DPKG_ROOT': str(fake_root)}
            postinst = stage / 'DEBIAN/postinst'
            prerm = stage / 'DEBIAN/prerm'
            self.assertTrue(prerm.is_file(), 'package must install a removal hook for owned-policy cleanup')
            result = subprocess.run([postinst, 'configure'], env=env, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0, 'install must fail rather than replace an administrator policy')
            self.assertEqual(admin_policy.read_text(), 'administrator policy\n')
            subprocess.run([prerm, 'remove'], env=env, check=True)
            self.assertEqual(admin_policy.read_text(), 'administrator policy\n', 'remove must not touch administrator policy')
            admin_policy.unlink()
            protected = root / 'admin-policy.apparmor'
            protected.write_text('external administrator policy\n')
            admin_policy.symlink_to(protected)
            result = subprocess.run([postinst, 'configure'], env=env, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0, 'install must not replace an administrator symlink')
            subprocess.run([prerm, 'remove'], env=env, check=True)
            self.assertTrue(admin_policy.is_symlink())
            self.assertEqual(admin_policy.read_text(), 'external administrator policy\n')

if __name__=='__main__':unittest.main()
