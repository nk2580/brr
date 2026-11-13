#!/usr/bin/env python3
"""Lightweight static checks for the Linux release scripts; does not build."""
from pathlib import Path
import subprocess
import json
import os
import tempfile
import unittest
import hashlib
import re

ROOT = Path(__file__).resolve().parents[2]
LINUX = ROOT / "packaging/linux"


def run_mocked_release_build(temp, sidecar_mode, existing_tuple=None, archive_browser_pin=None):
    temp = Path(temp)
    repo = temp / "repo"
    fake_bin = temp / "bin"
    output = temp / "output"
    (repo / "packaging/linux").mkdir(parents=True)
    (repo / "scripts").mkdir()
    fake_bin.mkdir()
    (repo / "package.json").write_text('{"version":"0.1.0"}\n')
    (repo / "packaging/linux/Dockerfile.build").write_text("FROM scratch\n")
    if archive_browser_pin is None:
        archive_browser_pin = {
            "version": "archive-browser",
            "assets": [{"platform": "linux64", "sha256": "archive-pinned"}],
        }
    (repo / "packaging/browser-artifacts.json").write_text(
        '{"version":"browser-test","assets":[{"platform":"linux64","sha256":"pinned-browser"}]}\n'
    )
    script = repo / "packaging/linux/build-release.sh"
    script.write_text((LINUX / "build-release.sh").read_text())
    script.chmod(0o755)

    node = fake_bin / "node"
    node.write_text(
        "#!/bin/sh\n"
        "case \"$1\" in\n"
        "  *prepare-source-release.mjs) python3 - \"$2\" <<'PY'\n"
        "import io, json, os, tarfile, sys\n"
        "data=json.dumps(json.loads(os.environ['ARCHIVE_BROWSER_PIN'])).encode()\n"
        "with tarfile.open(sys.argv[1], 'w:gz') as archive:\n"
        " member=tarfile.TarInfo('packaging/browser-artifacts.json')\n"
        " member.size=len(data)\n"
        " archive.addfile(member, io.BytesIO(data))\n"
        "PY\n"
        "    ;;\n"
        "esac\n"
    )
    node.chmod(0o755)
    docker = fake_bin / "docker"
    docker.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "case \"$1\" in\n"
        "  image|create|start|rm) exit 0 ;;\n"
        "  cp)\n"
        "    case \"$2\" in\n"
        "      *:/work/output/.)\n"
        "        out=\"${3%/}\"\n"
        "        mkdir -p \"$out\"\n"
        "        package=\"$out/brr-node_0.1.0_amd64.deb\"\n"
        "        printf package-fixture > \"$package\"\n"
        "        if [ \"$PACKAGE_SIDECAR_MODE\" = correct ]; then\n"
        "          digest=$(python3 -c \"import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())\" \"$package\")\n"
        "        else digest=$(printf '0%.0s' $(seq 1 64)); fi\n"
        "        printf '%s  %s\\n' \"$digest\" \"$(basename \"$package\")\" > \"$package.sha256\"\n"
        "        ;;\n"
        "    esac\n"
        "    exit 0 ;;\n"
        "  *) exit 99 ;;\n"
        "esac\n"
    )
    docker.chmod(0o755)
    env = os.environ.copy()
    env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
    env["PACKAGE_SIDECAR_MODE"] = sidecar_mode
    env["ARCHIVE_BROWSER_PIN"] = json.dumps(archive_browser_pin)
    if existing_tuple:
        output.mkdir(parents=True)
        for name, contents in existing_tuple.items():
            (output / name).write_bytes(contents)
    result = subprocess.run(
        ["bash", str(script), str(output), "0.1.0"], env=env, capture_output=True, text=True,
    )
    return result, output


class LinuxReleaseScripts(unittest.TestCase):
    def test_release_build_rejects_package_bytes_that_do_not_match_sidecar(self):
        with tempfile.TemporaryDirectory(prefix="brr-release-deb-mismatch-") as temp:
            result, output = run_mocked_release_build(temp, "wrong")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("package checksum mismatch", result.stderr)
            for name in [
                "brr-source-0.1.0.tar.gz", "brr-source-0.1.0.tar.gz.sha256",
                "brr-node_0.1.0_amd64.deb", "brr-node_0.1.0_amd64.deb.sha256",
                "brr-linux-build.json",
            ]:
                self.assertFalse((output / name).exists(), f"failed build published {name}")
            self.assertEqual(list(output.iterdir()), [])

    def test_failed_same_version_rebuild_preserves_prior_release_tuple(self):
        previous = {
            "brr-source-0.1.0.tar.gz": b"old-source-archive",
            "brr-source-0.1.0.tar.gz.sha256": b"old-source-checksum",
            "brr-node_0.1.0_amd64.deb": b"old-package",
            "brr-node_0.1.0_amd64.deb.sha256": b"old-package-checksum",
            "brr-linux-build.json": b"old-manifest",
        }
        with tempfile.TemporaryDirectory(prefix="brr-release-prior-tuple-") as temp:
            result, output = run_mocked_release_build(temp, "wrong", existing_tuple=previous)
            self.assertNotEqual(result.returncode, 0)
            for name, contents in previous.items():
                self.assertEqual((output / name).read_bytes(), contents, name)

    def test_release_manifest_records_verified_deb_sha256_and_preserves_fields(self):
        with tempfile.TemporaryDirectory(prefix="brr-release-deb-hash-") as temp:
            result, output = run_mocked_release_build(temp, "correct")
            self.assertEqual(result.returncode, 0, result.stderr)
            package = output / "brr-node_0.1.0_amd64.deb"
            manifest = json.loads((output / "brr-linux-build.json").read_text())
            self.assertEqual(manifest["debianPackageSha256"], hashlib.sha256(package.read_bytes()).hexdigest())
            self.assertEqual(manifest["debianPackage"], package.name)
            self.assertEqual(manifest["browserVersion"], "archive-browser")
            self.assertEqual(manifest["browserArchiveSha256"], "archive-pinned")
            self.assertNotEqual(manifest["browserVersion"], "browser-test")
            recipe_sha = hashlib.sha256((Path(temp) / "repo/packaging/linux/Dockerfile.build").read_bytes()).hexdigest()
            self.assertEqual(manifest["builderRecipeSha256"], recipe_sha)
            self.assertTrue(manifest["containerImage"].endswith(recipe_sha[:12]))
            self.assertEqual(
                {path.name for path in output.iterdir()},
                {
                    "brr-source-0.1.0.tar.gz", "brr-source-0.1.0.tar.gz.sha256",
                    "brr-node_0.1.0_amd64.deb", "brr-node_0.1.0_amd64.deb.sha256",
                    "brr-linux-build.json",
                },
            )
            for key in [
                "architecture", "target", "containerImage", "builderRecipeSha256",
                "sourceArchive", "sourceArchiveSha256", "sourceDateEpoch", "browserVersion",
                "browserArchiveSha256",
            ]:
                self.assertIn(key, manifest)

    def test_build_is_amd64_pinned_and_uses_isolated_work_directory(self):
        script = (LINUX / "build-release.sh").read_text()
        dockerfile = (LINUX / "Dockerfile.build").read_text()
        self.assertIn("--platform linux/amd64", script)
        self.assertIn("x86_64-unknown-linux-gnu", script)
        self.assertIn("node scripts/build-runtime.mjs --release", script)
        self.assertIn("node scripts/build-node.mjs --release", script)
        self.assertIn("packaging/build-deb.py", script)
        self.assertIn("/work/source", script)
        self.assertNotIn("/src:", script)
        self.assertIn("@sha256:", dockerfile)
        self.assertIn("RUSTUP_TOOLCHAIN=1.98.0", dockerfile)

    def test_verifier_is_disposable_and_covers_upgrade_and_user_service(self):
        script = (LINUX / "verify-install.sh").read_text()
        self.assertIn("Refusing to install outside a container", script)
        self.assertIn("debian:12|ubuntu:24.04", script)
        self.assertIn("dpkg-deb --build", script)
        self.assertIn("apt-get install", script)
        self.assertIn("fleet init", script)
        self.assertIn(" pair --endpoint", script)
        self.assertIn("service install --start", script)
        self.assertIn("service uninstall", script)
        self.assertIn("uninstalled-service-status.txt", script)
        self.assertIn("preserve-me.txt", script)
        self.assertIn("systemctl is-system-running", script)
        self.assertIn('"/run/user/$TEST_UID/systemd/private"', script)
        self.assertNotIn('"/run/user/$TEST_UID/bus"', script)
        self.assertNotIn('DBUS_SESSION_BUS_ADDRESS="', script)
        self.assertNotIn("readelf", script)
        self.assertIn("verify_amd64_elf", script)
        self.assertIn("--inside-vm", script)
        self.assertIn("systemd-detect-virt --vm", script)
        self.assertIn("systemd-detect-virt --container", script)
        self.assertIn("BRR_DISPOSABLE_VM_INSTALL_QA_V1", script)
        self.assertIn("brr-check", script)
        for path in ["/home/brr-check", "/var/mail/brr-check", "/var/spool/mail/brr-check"]:
            self.assertIn(path, script)

    def test_verifier_imports_an_unused_draft_attachment_and_checks_its_canonical_backup(self):
        script = (LINUX / "verify-install.sh").read_text()
        import_at = script.index("request chat attachments.importBytes")
        init_at = script.index('fleet init > "$TEMP_ROOT/fleet-init.json"')
        self.assertLess(import_at, init_at)
        self.assertIn('ATTACHMENT_OWNER="qa-unused-draft-attachment"', script)
        self.assertIn("request chat attachments.list", script)
        self.assertIn("request chat attachments.preview", script)
        self.assertIn("engine/agents/chat/chat.sqlite3", script)
        self.assertIn("files/app-attachment-blob/", script)
        self.assertIn("verify_attachment_store after-service-uninstall", script)
        self.assertIn("verify_attachment_store after-package-purge", script)

        after_init = script.split('fleet init > "$TEMP_ROOT/fleet-init.json"', 1)[1]
        checker = re.search(r"<<'PY'\n(.*?)\nPY", after_init, flags=re.DOTALL)
        self.assertIsNotNone(checker, "Fleet backup verification should use the actual embedded Python check")
        code = checker.group(1)
        persistence_section = script.split('  verify_attachment_store() {', 1)[1].split('\n  }', 1)[0]
        persistence_checker = re.search(r"<<'PY'\n(.*?)\nPY", persistence_section, flags=re.DOTALL)
        self.assertIsNotNone(persistence_checker, "Post-uninstall attachment checks should use the embedded read-only verifier")

        with tempfile.TemporaryDirectory(prefix="brr-attachment-backup-check-") as temp:
            temp = Path(temp)
            data = temp / "data"
            backup = data / "node/import-backups/backup-fixture"
            backup.mkdir(parents=True)
            (data / "node/import-backups").mkdir(parents=True, exist_ok=True)
            attachment_id, owner = "attachment-fixture", "qa-unused-draft-attachment"
            name, text = "canonical-backup-check.txt", "Brr canonical attachment backup fixture v1"
            payload = (text + "\n").encode()
            blob_id = "blob-fixture"
            chat = backup / "databases/chat.sqlite3"
            chat.parent.mkdir(parents=True)
            connection = __import__("sqlite3").connect(chat)
            connection.executescript(
                "CREATE TABLE chat_attachments(id TEXT,blob_id TEXT,name TEXT,mime TEXT,size INTEGER);"
                "CREATE TABLE chat_attachment_grants(owner_id TEXT,attachment_id TEXT);"
            )
            connection.execute("INSERT INTO chat_attachments VALUES(?,?,?,?,?)", (
                attachment_id, blob_id, name, "text/plain", len(payload),
            ))
            connection.execute("INSERT INTO chat_attachment_grants VALUES(?,?)", (owner, attachment_id))
            connection.commit()
            connection.close()
            chat_bytes = chat.read_bytes()
            digest = hashlib.sha256(payload).hexdigest()
            blob_relative = f"files/app-attachment-blob/{blob_id}"
            blob = backup / blob_relative
            blob.parent.mkdir(parents=True)
            blob.write_bytes(payload)
            (backup / "manifest.json").write_text(json.dumps({
                "databases": [{
                    "name": "chat", "relativePath": "databases/chat.sqlite3",
                    "sizeBytes": len(chat_bytes), "sha256": hashlib.sha256(chat_bytes).hexdigest(),
                }],
                "files": [{
                    "relativePath": blob_relative, "classification": "app-attachment-blob",
                    "sizeBytes": len(payload), "sha256": digest,
                }],
            }))
            init = temp / "fleet-init.json"
            init.write_text(json.dumps({"backupDirectory": str(backup)}))
            args = ["python3", "-", str(init), str(data), attachment_id, owner, name, text, digest]
            result = subprocess.run(args, input=code, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            blob.write_bytes(b"altered")
            rejected = subprocess.run(args, input=code, capture_output=True, text=True)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("attachment bytes", rejected.stderr)

            data = temp / "persistent-data"
            chat_root = data / "engine/agents/chat"
            blobs = chat_root / "blobs"
            blobs.mkdir(parents=True)
            chat = chat_root / "chat.sqlite3"
            connection = __import__("sqlite3").connect(chat)
            connection.executescript(
                "CREATE TABLE chat_attachments(id TEXT,blob_id TEXT,name TEXT,mime TEXT,size INTEGER);"
                "CREATE TABLE chat_attachment_grants(owner_id TEXT,attachment_id TEXT);"
            )
            connection.execute("INSERT INTO chat_attachments VALUES(?,?,?,?,?)", (
                attachment_id, blob_id, name, "text/plain", len(payload),
            ))
            connection.execute("INSERT INTO chat_attachment_grants VALUES(?,?)", (owner, attachment_id))
            connection.commit()
            connection.close()
            (blobs / blob_id).write_bytes(payload)
            persistence_args = [
                "python3", "-", str(data), attachment_id, owner, name, text,
                "after-package-purge",
            ]
            persisted = subprocess.run(
                persistence_args, input=persistence_checker.group(1), capture_output=True, text=True,
            )
            self.assertEqual(persisted.returncode, 0, persisted.stderr)
            (blobs / blob_id).write_bytes(b"purged")
            changed = subprocess.run(
                persistence_args, input=persistence_checker.group(1), capture_output=True, text=True,
            )
            self.assertNotEqual(changed.returncode, 0)
            self.assertIn("blob bytes changed", changed.stderr)

    def test_verifier_amd64_elf_header_check_accepts_and_rejects_binary_fixtures(self):
        script = (LINUX / "verify-install.sh").read_text()
        function = script.split("verify_amd64_elf() {", 1)[1].split("\n}", 1)[0]
        match = re.search(r"<<'PY'\n(.*?)\nPY", function, flags=re.DOTALL)
        self.assertIsNotNone(match, "ELF verification should use an embedded Python stdlib check")
        checker = match.group(1)

        with tempfile.TemporaryDirectory(prefix="brr-elf-header-") as temp:
            temp = Path(temp)
            valid = bytearray(64)
            valid[:4] = b"\x7fELF"
            valid[4:7] = bytes([2, 1, 1])
            valid[18:20] = (62).to_bytes(2, "little")
            valid_path = temp / "valid-elf"
            valid_path.write_bytes(valid)
            valid_result = subprocess.run(
                ["python3", "-", str(valid_path)], input=checker, capture_output=True, text=True,
            )
            self.assertEqual(valid_result.returncode, 0, valid_result.stderr)

            for name, data in [
                ("wrong-machine", bytes(valid[:18]) + (3).to_bytes(2, "little")),
                ("wrong-class", bytes(valid[:4]) + bytes([1]) + bytes(valid[5:])),
                ("short-header", bytes(valid[:63])),
                ("truncated", b"\x7fELF"),
                ("not-elf", b"nope" + bytes(valid[4:])),
            ]:
                path = temp / name
                path.write_bytes(data)
                result = subprocess.run(
                    ["python3", "-", str(path)], input=checker, capture_output=True, text=True,
                )
                self.assertNotEqual(result.returncode, 0, name)

    def test_verifier_creates_systemd_user_xdg_directories_as_test_user(self):
        script = (LINUX / "verify-install.sh").read_text()
        self.assertIn(
            'runuser -u "$TEST_USER" -- mkdir -p "$TEST_CONFIG" "$TEST_XDG"',
            script,
        )
        self.assertNotIn('mkdir -p "$TEST_DATA" "$TEST_CONFIG" "$TEST_XDG"', script)
        self.assertIn('install -d -o "$TEST_USER" -g "$TEST_USER" -m 0700 "$TEST_DATA"', script)
        self.assertNotIn('chown -R "$TEST_USER:$TEST_USER" "$TEMP_ROOT"', script)
        self.assertIn('SANDBOX_ROOT="$TEST_HOME/sandbox-service"', script)
        self.assertNotIn('SANDBOX_ROOT="$TEMP_ROOT/sandbox-service"', script)

    def test_native_vm_entry_rejects_wrong_environment_before_package_mutation(self):
        with tempfile.TemporaryDirectory(prefix="brr-native-vm-guard-") as temp:
            temp = Path(temp)
            fake_bin = temp / "bin"
            fake_bin.mkdir()
            package = temp / "brr-node.deb"
            package.write_bytes(b"test-only package marker")
            marker = temp / "missing-qa-marker"
            calls = temp / "calls.log"

            def stub(name, source):
                path = fake_bin / name
                path.write_text("#!/bin/sh\n" + source + "\n")
                path.chmod(0o755)

            stub("id", 'echo "$*" >> "$QA_CALLS"; [ "$1" = -u ] && { echo 0; exit 0; }; exit 0')
            stub("systemd-detect-virt", 'echo "$*" >> "$QA_CALLS"; if [ "$1" = --container ]; then exit "$QA_CONTAINER"; fi; if [ "$1" = --vm ]; then [ "$QA_VM" = 0 ] && { echo kvm; exit 0; }; exit 1; fi; exit 2')
            stub("apt-get", 'echo "apt-get $*" >> "$QA_CALLS"; exit 0')
            env = os.environ.copy()
            env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
            env["QA_CALLS"] = str(calls)
            env["QA_CONTAINER"] = "1"  # systemd-detect-virt --container reports not a container

            env["QA_VM"] = "1"
            non_vm = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(non_vm.returncode, 2)
            self.assertIn("disposable VM", non_vm.stderr)
            self.assertNotIn("apt-get", calls.read_text())

            calls.unlink()
            env["QA_VM"] = "0"
            missing_marker = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(missing_marker.returncode, 2)
            self.assertIn("QA marker", missing_marker.stderr)
            self.assertNotIn("apt-get", calls.read_text())

            calls.unlink()
            env["QA_CONTAINER"] = "0"  # systemd-detect-virt --container reports a container
            container = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(container.returncode, 2)
            self.assertIn("inside a container", container.stderr)
            self.assertNotIn("apt-get", calls.read_text())

    def test_native_vm_entry_rejects_any_package_record_and_orphaned_group(self):
        with tempfile.TemporaryDirectory(prefix="brr-native-vm-state-guard-") as temp:
            temp = Path(temp)
            fake_bin = temp / "bin"
            fake_bin.mkdir()
            package = temp / "brr-node.deb"
            package.write_bytes(b"test-only package marker")
            marker = temp / "qa-marker"
            marker.write_text("BRR_DISPOSABLE_VM_INSTALL_QA_V1\n")
            marker.chmod(0o600)
            calls = temp / "calls.log"

            def stub(name, source):
                path = fake_bin / name
                path.write_text("#!/bin/sh\n" + source + "\n")
                path.chmod(0o755)

            stub("id", 'echo "$*" >> "$QA_CALLS"; [ "$1" = -u ] && { echo 0; exit 0; }; exit 0')
            stub("systemd-detect-virt", 'echo "$*" >> "$QA_CALLS"; if [ "$1" = --container ]; then exit 1; fi; if [ "$1" = --vm ]; then echo kvm; exit 0; fi; exit 2')
            stub("stat", 'echo "$*" >> "$QA_CALLS"; echo 0:600')
            stub("dpkg-query", 'echo "$*" >> "$QA_CALLS"; echo "deinstall ok config-files"; exit 0')
            stub("getent", 'echo "$*" >> "$QA_CALLS"; exit 2')
            stub("apt-get", 'echo "apt-get $*" >> "$QA_CALLS"; exit 0')
            env = os.environ.copy()
            env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
            env["QA_CALLS"] = str(calls)

            package_record = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(package_record.returncode, 2)
            self.assertIn("package record", package_record.stderr)
            self.assertNotIn("apt-get", calls.read_text())

            calls.unlink()
            stub("dpkg-query", 'echo "$*" >> "$QA_CALLS"; exit 1')
            stub("getent", 'echo "$*" >> "$QA_CALLS"; if [ "$1" = group ] && [ "$2" = brr-check ]; then echo brr-check; exit 0; fi; exit 2')
            orphaned_group = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(orphaned_group.returncode, 2)
            self.assertIn("brr-check group", orphaned_group.stderr)
            self.assertNotIn("apt-get", calls.read_text())

    def test_native_upgrade_argument_requires_absolute_regular_prior_deb_before_mutation(self):
        with tempfile.TemporaryDirectory(prefix="brr-native-prior-package-") as temp:
            temp = Path(temp)
            fake_bin = temp / "bin"
            fake_bin.mkdir()
            package = temp / "brr-node-new.deb"
            package.write_bytes(b"new package fixture")
            marker = temp / "qa-marker"
            marker.write_text("BRR_DISPOSABLE_VM_INSTALL_QA_V1\n")
            marker.chmod(0o600)
            calls = temp / "calls.log"

            def stub(name, source):
                path = fake_bin / name
                path.write_text("#!/bin/sh\n" + source + "\n")
                path.chmod(0o755)

            stub("id", 'echo "$*" >> "$QA_CALLS"; [ "$1" = -u ] && { echo 0; exit 0; }; exit 0')
            stub("systemd-detect-virt", 'echo "$*" >> "$QA_CALLS"; if [ "$1" = --container ]; then exit 1; fi; if [ "$1" = --vm ]; then echo kvm; exit 0; fi; exit 2')
            stub("stat", 'echo "$*" >> "$QA_CALLS"; echo 0:600')
            stub("dpkg-query", 'echo "$*" >> "$QA_CALLS"; exit 1')
            stub("getent", 'echo "$*" >> "$QA_CALLS"; exit 2')
            stub("apt-get", 'echo "apt-get $*" >> "$QA_CALLS"; exit 0')
            stub("dpkg-deb", 'echo "dpkg-deb $*" >> "$QA_CALLS"; exit 1')
            env = os.environ.copy()
            env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
            env["QA_CALLS"] = str(calls)

            result = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker), "relative-old.deb"],
                env=env, capture_output=True, text=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("previous package path must be absolute", result.stderr.lower())
            self.assertNotIn("apt-get", calls.read_text())
            self.assertNotIn("dpkg-deb", calls.read_text())

            calls.unlink()
            target = temp / "previous-target.deb"
            target.write_bytes(b"prior package fixture")
            previous_link = temp / "brr-node-old.deb"
            previous_link.symlink_to(target)
            symlink_result = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker), str(previous_link)],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(symlink_result.returncode, 2)
            self.assertIn("regular non-symlink", symlink_result.stderr.lower())
            self.assertNotIn("apt-get", calls.read_text())
            self.assertNotIn("dpkg-deb", calls.read_text())

    def test_native_upgrade_requires_matching_amd64_packages_and_strictly_older_version(self):
        with tempfile.TemporaryDirectory(prefix="brr-native-prior-version-") as temp:
            temp = Path(temp)
            fake_bin = temp / "bin"
            fake_bin.mkdir()
            package = temp / "brr-node-new.deb"
            package.write_bytes(b"new package fixture")
            previous = temp / "brr-node-previous.deb"
            previous.write_bytes(b"previous package fixture")
            marker = temp / "qa-marker"
            marker.write_text("BRR_DISPOSABLE_VM_INSTALL_QA_V1\n")
            marker.chmod(0o600)
            calls = temp / "calls.log"

            def stub(name, source):
                path = fake_bin / name
                path.write_text("#!/bin/sh\n" + source + "\n")
                path.chmod(0o755)

            stub("id", 'echo "$*" >> "$QA_CALLS"; [ "$1" = -u ] && { echo 0; exit 0; }; exit 0')
            stub("systemd-detect-virt", 'echo "$*" >> "$QA_CALLS"; if [ "$1" = --container ]; then exit 1; fi; if [ "$1" = --vm ]; then echo kvm; exit 0; fi; exit 2')
            stub("stat", 'echo "$*" >> "$QA_CALLS"; echo 0:600')
            stub("dpkg-deb", 'echo "dpkg-deb $*" >> "$QA_CALLS"; [ "$1" = -f ] || exit 1; if [ "$2" = "$QA_PREVIOUS" ]; then name=$QA_PREVIOUS_NAME; version=$QA_PREVIOUS_VERSION; arch=$QA_PREVIOUS_ARCH; else name=${QA_PACKAGE_NAME:-brr-node}; version=0.1.0-1; arch=${QA_PACKAGE_ARCH:-amd64}; fi; case "$3" in Package) echo "$name" ;; Version) echo "$version" ;; Architecture) echo "$arch" ;; *) exit 1 ;; esac')
            stub("dpkg", 'echo "dpkg $*" >> "$QA_CALLS"; [ "$1" = --compare-versions ] || exit 2; [ "$2" = "$QA_PREVIOUS_VERSION" ] && [ "$3" = lt ] && [ "$4" = 0.1.0-1 ] && exit 1; exit 2')
            stub("apt-get", 'echo "apt-get $*" >> "$QA_CALLS"; exit 0')
            env = os.environ.copy()
            env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
            env["QA_CALLS"] = str(calls)
            env["QA_PREVIOUS"] = str(previous)
            env["QA_PREVIOUS_NAME"] = "brr-node"
            env["QA_PREVIOUS_VERSION"] = "0.1.0-2"
            env["QA_PREVIOUS_ARCH"] = "amd64"

            result = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker), str(previous)],
                env=env, capture_output=True, text=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("previous package version must be older", result.stderr.lower())
            self.assertIn("dpkg --compare-versions 0.1.0-2 lt 0.1.0-1", calls.read_text())
            self.assertNotIn("apt-get", calls.read_text())

            for key, value, message in [
                ("QA_PREVIOUS_NAME", "not-brr-node", "both be named brr-node"),
                ("QA_PREVIOUS_ARCH", "arm64", "both have amd64 architecture"),
                ("QA_PACKAGE_NAME", "not-brr-node", "both be named brr-node"),
                ("QA_PACKAGE_ARCH", "arm64", "both have amd64 architecture"),
            ]:
                with self.subTest(key=key, value=value):
                    calls.unlink()
                    env.update({
                        "QA_PREVIOUS_NAME": "brr-node", "QA_PREVIOUS_VERSION": "0.1.0-2",
                        "QA_PREVIOUS_ARCH": "amd64", "QA_PACKAGE_NAME": "brr-node",
                        "QA_PACKAGE_ARCH": "amd64",
                    })
                    env[key] = value
                    invalid_metadata = subprocess.run(
                        ["bash", str(LINUX / "verify-install.sh"), "--inside-vm", str(package), str(marker), str(previous)],
                        env=env, capture_output=True, text=True,
                    )
                    self.assertEqual(invalid_metadata.returncode, 2)
                    self.assertIn(message, invalid_metadata.stderr.lower())
                    self.assertNotIn("dpkg --compare-versions", calls.read_text())
                    self.assertNotIn("apt-get", calls.read_text())

    def test_shell_syntax(self):
        for path in [LINUX / "build-release.sh", LINUX / "verify-install.sh"]:
            result = subprocess.run(["bash", "-n", str(path)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_verifier_uses_private_cgroups_without_host_cgroup_bind(self):
        with tempfile.TemporaryDirectory(prefix="brr-verifier-argv-") as temp:
            temp = Path(temp)
            package = temp / "brr-node.deb"
            package.write_bytes(b"test-only package marker")
            capture = temp / "docker-argv.jsonl"
            fake_bin = temp / "bin"
            fake_bin.mkdir()
            docker = fake_bin / "docker"
            docker.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "args = sys.argv[1:]\n"
                "with open(os.environ['DOCKER_ARGV_CAPTURE'], 'a') as out:\n"
                "    out.write(json.dumps(args) + '\\n')\n"
                "if args[:1] == ['exec'] and 'systemctl' in args and 'is-system-running' in args:\n"
                "    print('running')\n"
            )
            docker.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
            env["DOCKER_ARGV_CAPTURE"] = str(capture)
            result = subprocess.run(
                ["bash", str(LINUX / "verify-install.sh"), str(package), "ubuntu:24.04"],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in capture.read_text().splitlines()]
            run = next(call for call in calls if call[0] == "run")
            self.assertIn("--cgroupns", run)
            self.assertEqual(run[run.index("--cgroupns") + 1], "private")
            self.assertIn("--tmpfs", run)
            self.assertEqual(run[run.index("--tmpfs") + 1], "/run")
            self.assertEqual(run[run.index("--tmpfs") + 3], "/run/lock")
            self.assertFalse(any("/sys/fs/cgroup" in arg for arg in run))


if __name__ == "__main__":
    unittest.main()
