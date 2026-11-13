# Set up Brr fleet nodes

Choose a primary to coordinate the fleet. The primary and workers need to reach
each other over a LAN or private VPN. Each node keeps its own workspaces and
ChatGPT login.

## Install the node

On Debian 12 or Ubuntu 24.04 (amd64), use the
[Linux build instructions](../packaging/linux/README.md) to create a Debian
package. Verify the checksum before installation. For the default package
version, run these commands in the release directory:

```sh
sha256sum -c brr-node_0.1.0_amd64.deb.sha256
sudo apt install ./brr-node_0.1.0_amd64.deb
```

You can also build the headless service on an Apple silicon Mac. From the
recursive checkout, run `pnpm runtime:build --release` and `pnpm node:build --release`.
The resulting `brrd` and `brrctl` are in `src-tauri/binaries`. Use the complete
staged directory so the service can find its sibling runtime and browser.
Give the standalone Mac node its own data directory so it can run alongside
the desktop app. In each Mac shell used to run the commands below, set:

```sh
export PATH="$PWD/src-tauri/binaries:$PATH"
export BRR_DATA_DIR="$HOME/Library/Application Support/dev.brr.fleet"
```

## Start and enroll

The service listens on loopback by default. Before starting it as a user
service, run it once with the machine's private address and advertised HTTPS
endpoint. Run as your regular account. Use a different address and name on
each machine.

```sh
brrd --name "Research node" --listen 192.168.1.12:8787 \
  --advertise-host 192.168.1.12 --endpoint https://192.168.1.12:8787
```

Once it starts, press Ctrl+C. The service saves these settings in
`node/service.json` under its data directory. If the service is already
running, stop it before changing these settings. Install and start the user
service:

```sh
brrctl service install --start
brrctl status
```

On Linux, a user service normally stops when the user manager shuts down.
An administrator can enable lingering for the service account with
`loginctl enable-linger <username>`.

On the primary:

```sh
brrctl fleet init
(umask 077; brrctl fleet invite --name "Linux worker" > worker-invitation.json)
```

Transfer the invitation privately to the worker and run
`brrctl fleet join --invitation-file worker-invitation.json`. The invitation
expires and can be redeemed once. Remove the invitation file after enrollment.

On each execution node, run `brrctl account login` as the service account
and complete the ChatGPT device-code flow. Check enrollment with
`brrctl fleet status` and request a sync with `brrctl fleet sync`.

Use matching node releases before sharing saved attachments. Accepted
attachments sync with published conversation history. Workspaces, tasks,
schedules, and active execution stay on their owner.
