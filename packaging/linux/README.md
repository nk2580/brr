# Linux release build and install checks

The Linux package is an `amd64` `.deb` built from the selected source archive
inside the project’s pinned Rust 1.98 Bookworm image. The build also uses the
Linux browser version and per-file hashes in
[`../browser-artifacts.json`](../browser-artifacts.json). It writes a source
archive and SHA-256, the `.deb` and SHA-256, and a JSON record of the source,
package, container, target, and browser inputs. The build verifies the two
checksums in a private staging directory and writes `brr-linux-build.json`
last. Use that manifest to identify a complete local release. A failed build
or checksum check leaves the previous release files available.

Build from a recursive Git checkout with Docker running:

```sh
packaging/linux/build-release.sh
```

The default output is `.cache/releases/linux`. Pass an output directory and
Debian version as positional arguments to override it:

```sh
packaging/linux/build-release.sh /tmp/brr-release 0.1.0
```

The builder creates a uniquely named container and copies the source archive
into that container’s `/work` directory. It does not bind or reuse `/src`,
`src-tauri/binaries`, or the Rust target directory of another build. The
container targets `x86_64-unknown-linux-gnu`, runs the owned runtime and node
builders with `--release`, stages the pinned Linux browser, then calls
`packaging/build-deb.py`. This command requires an available Docker engine and
network access for Rust crates and the checksum-pinned V8/browser downloads.

Run install, upgrade, service, IPC, pairing, and removal checks in separate
disposable systemd containers for both supported test distributions:

```sh
packaging/linux/verify-install.sh \
  .cache/releases/linux/brr-node_0.1.0_amd64.deb debian:12-slim
packaging/linux/verify-install.sh \
  .cache/releases/linux/brr-node_0.1.0_amd64.deb ubuntu:24.04
```

The verifier makes a lower-version copy of the same `.deb` for the upgrade
step, resolves dependencies through the distro package manager, checks every
installed executable is an amd64 ELF, and runs the node under a temporary
nonroot account. It waits for the packaged owned runtime to report ready,
requires an explicitly logged-out ChatGPT account, and verifies a chat
inference request is rejected at that authentication boundary. It exercises
the packaged Chromium through the local IPC `browser_request` path by opening
and snapshotting `about:blank` as that same nonroot user; Chromium runs with
its normal sandbox flags, and a launch or IPC failure fails verification.

The package depends on Bubblewrap 0.8.0 or newer. The verifier checks for the
runtime-required `--as-pid-1` and `--perms` flags, then invokes the installed
`codex-linux-sandbox` alias for `brr-runtime`. The sandbox must open a selected
original read-only and let the command write in its workspace, while hiding
an adjacent private store under the same denied service root. Debian 12 ships
Bubblewrap 0.8.0, which lacks newer `--argv0` and `--ro-bind-fd` options; the
runtime has compatibility handling for those omissions. Ubuntu 24.04 ships
Bubblewrap 0.9.0.

The pairing check creates a disposable mobile invitation through local IPC,
then redeems it over HTTPS using the invitation CA certificate and SHA-256
pin. It uses the returned temporary bearer credential to fetch `/v1/snapshot`
and checks the authenticated node identity, protocol version, cursor, and node
inventory. No account credentials or persistent user data are used. The check
also covers fleet initialization/status, per-user service
install/start/status/stop/uninstall, and package purge. A private sentinel
confirms package removal leaves the test user’s data directory intact. The
isolated account and data are deleted when the check exits.

To run the same full package checks directly in a disposable systemd VM, copy
the package and verifier into that VM, then create the one-time marker there
and invoke the native entry point as root:

```sh
sudo install -o root -g root -m 0600 /dev/null /run/brr-disposable-vm-qa
printf 'BRR_DISPOSABLE_VM_INSTALL_QA_V1\n' | sudo tee /run/brr-disposable-vm-qa >/dev/null
sudo ./verify-install.sh --inside-vm ./brr-node_0.1.0_amd64.deb /run/brr-disposable-vm-qa
```

Create the marker only inside the disposable VM. The native entry point
requires root, `systemd-detect-virt --vm`, a negative
`systemd-detect-virt --container` result, the exact root-owned mode-0600 marker
and content shown above, Debian 12 or Ubuntu 24.04 with systemd running, and a
clean package/user/path preflight before it runs `apt-get`. It then executes
the same upgrade, runtime, browser sandbox, IPC, TLS pairing, service, purge,
and data-preservation checks as the container path. It refuses to run if
`brr-node`, the `brr-check` account, or known Brr package paths already exist.
Installed amd64 ELF headers are checked with Python's standard library. The
user-service check waits for systemd's `/run/user/<uid>/systemd/private` socket
and runs `systemctl --user` as the disposable account without setting a D-Bus
session address or overriding that account's home directory.

These checks fail when runtime readiness, logged-out inference rejection,
Chromium sandbox startup, pinned TLS redemption, authenticated snapshot
identity, or IPC verification cannot be established. A privileged systemd
container verifies the package ABI and exercises the packaged browser, but it
does not prove that the VM host policy permits Chromium user namespaces. A
Debian container on an Ubuntu kernel can still be blocked by Ubuntu's AppArmor
user-namespace policy: nested Bubblewrap can fail while acquiring `net_admin`
even though the container userspace is supported. Run the native verifier in a
disposable VM with the target kernel and actual AppArmor configuration, and
record `apparmor_status`, loaded Brr profiles, and a real browser startup.

The scripts produce local release artifacts. Distribute the verified package,
source archive, build manifest, and checksums together.
