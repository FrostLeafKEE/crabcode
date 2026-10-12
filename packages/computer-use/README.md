# Shared Computer Use

Desktop, VS Code and CLI use the same native executor. Desktop and VS Code
also share the JavaScript host channel. Neither VS Code nor CLI requires
Crab Desktop.

```
Gateway ComputerUseBroker
  -> computer-use/client/src/channel.ts (Desktop / VS Code / local companion)
  -> authenticated loopback RPC
  -> computer-use/host (one per OS user, one native execution queue)
  -> computer-use/core (original Desktop engine)
```

The CLI binds Core's `ComputerUse` tool directly to this executor through a
small Python IPC adapter in `packages/cli/crabcode_cli/computer_use.py`. It
does not need Gateway, WebSocket, Node or either GUI client. Native capture,
input, AX ownership and delivery policies remain implemented only in `core`.

Source layout:

```
packages/computer-use/{core,host,client,scripts}
packages/vscode/computer-use/  # local UI extension for remote workspaces
packages/shared/              # unrelated cross-client chat utilities
```

`core` contains the original native implementation, background profiles and
regression fixtures. Only the Tauri entry points were replaced by ordinary Rust
exports. The Desktop VM guest also links this crate. Do not maintain a separate
VS Code implementation of capture, input, AX ownership or delivery policies.

`packages/computer-use/client/src/channel.ts` retains the original Desktop
Gateway protocol, model/monitor screenshot separation, preview retention and
release behavior. `channel.js` and `channel.d.ts` are checked-in generated files;
regenerate with `node packages/computer-use/client/build.mjs`.

## Installation and permissions

Each platform distribution bundles `host`. First enablement installs it in the
per-user application-data directory, under `CrabCode/ComputerUse`. macOS uses
`~/Library/Application Support/CrabCode/ComputerUse/Crab Computer Use.app`.
Grant that app Accessibility and Screen Recording access, then select refresh
in the client. Existing Desktop permissions do not transfer to the new app.
Local ad-hoc builds can require renewed authorization after recompilation.
If the switches are on but TCC logs report a code-requirement mismatch, reset
only this development component with `tccutil reset Accessibility
io.crabcode.computer-use` and `tccutil reset ScreenCapture
io.crabcode.computer-use`, then manually authorize the installed app again.
The application never resets or grants system permissions automatically.
Release builds must use the same `CRABCODE_SIGNING_IDENTITY` for both clients;
the native component must be included in the distribution's signing and
notarization process.

An exclusive OS lock prevents multiple executors. The endpoint descriptor is
private to the user and contains a random authentication token; do not log or
publish it. Only loopback connections are accepted. Every process start has a
new instance ID, so actions referring to a previous process are rejected.
Interrupted input requests are never replayed automatically.

Clients heartbeat their connection every 30 seconds. Native references for a
crashed connection expire after 120 seconds. Explicit release is scoped to the
client connection, session and agent. Upgrade waits for active leases/actions
to finish; it never terminates an active task. A different component build can
be installed when idle, followed by capability refresh.

For Remote SSH, WSL and dev containers, install `crabcode-computer-use` on the
local UI side. The main extension forwards the Gateway URL using
`vscode.env.asExternalUri`; the local companion owns native execution and the
monitor. A remote machine's desktop is not selected implicitly.

## CLI

The CLI automatically detects the installed component in both interactive and
pipe sessions. Run `crabcode` as usual; no session enablement flag is needed.
Capabilities determine whether Core exposes the tool. CLI follows the existing
`computer_use` settings and tool permission policy. With no installed component
or graphical session, ordinary coding remains available.

```sh
crabcode computer-use status
crabcode computer-use status --json
crabcode computer-use permissions  # macOS input permission settings
```

Desktop or VS Code's installed component is reused automatically. For a CLI-only
installation, build or obtain a bundle for the current OS/architecture and
install it once (the Python wheel does not contain the native binary):

```sh
node packages/computer-use/scripts/build.mjs
crabcode computer-use install --bundle packages/computer-use/dist/darwin-arm64
```

Replace `darwin-arm64` with the bundle's platform/architecture. On macOS,
`--bundle` also accepts the `.app` itself. After first-time installation, start
a new CLI session. Permission changes are detected during an existing session.
An SSH CLI controls the machine on which it runs; it does not forward to a
different desktop. VS Code's local companion is the supported remote UI route.

## Build and validation

From the repository root:

```sh
node packages/computer-use/scripts/build.mjs --debug
node packages/computer-use/client/build.mjs
cargo test --locked --manifest-path packages/computer-use/Cargo.toml --workspace
```

For VS Code extension-host development, first run
`npm run computer-use:build -- --debug` from `packages/vscode`; a JavaScript-only
watch rebuild does not produce the native resource. The local companion uses
the same command from its own package directory.

Desktop's Tauri build hook and both VS Code extensions' `vscode:prepublish`
scripts include the native component. Package platform-specific VSIX files;
build Windows binaries on Windows and macOS binaries on macOS, matching the
target architecture. `--out` selects a client resource directory. Release
builds are the default; `--debug` is intended for development only.

The core's ignored macOS tests exercise live input and need an unlocked GUI
and OS permissions. The opt-in `scripts/live-smoke.py` uses the real Gateway
broker, shared Node channel and installed executor against an isolated AppKit
fixture; it never types into an existing user document.
Use `--client cli` to exercise Core's tool with the CLI adapter and installed
executor, without starting Gateway or Node. The default `--client gateway`
tests the shared JavaScript channel.
