# Crab Desktop

Crab Desktop is the shared React client for local and remote CrabCode Gateways.
It runs either in a browser or inside the Tauri desktop shell.

## Development

```bash
cd packages/desktop
npm install

# Browser mode
npm run dev

# Tauri mode
npm run tauri dev
```

Browser mode opens at `http://127.0.0.1:1420`, stores connection and project UI
state in `localStorage`, and keeps passwords only in the current tab's
`sessionStorage`. It connects to an already-running Gateway. Tauri mode adds
system credential storage and automatic local Gateway installation/startup.

The bottom status bar stays visible during startup and in Settings. It shows
environment checks, live pip output, Gateway startup, connection progress, and
elapsed time. Click the status message to inspect the latest 100 log entries;
failed connections keep their error details and offer a retry action. Local
installation and authentication run on background workers so the desktop
window remains responsive while they are in progress.
The log also records the Gateway address and startup mode, plus the running
Gateway's version, package path, Python version and executable, environment
directory/type, platform, and startup directory. Runtime details are returned
by the authenticated workspace endpoint, so remote connections describe the
server's environment. Older Gateways can still connect without this metadata.
For local connections, packaged Desktop first checks the configured Python and
other detected Python environments for an existing CrabCode installation. It
reuses an installation only when its version matches Desktop, its Gateway
protocol and CLI/server dependencies pass checks, and the actual Gateway process
starts and passes its health check. Unusable candidates are skipped with a
diagnostic in the startup log. If none is usable, Desktop creates or reuses
`~/.crabcode/desktop/gateway-venv` and installs CrabCode there as needed; it does
not install into or upgrade external system, Homebrew, or Conda environments.
This rule applies to automatic provisioning. **Settings → General → CrabCode
Suite** provides a component checklist and install button. Gateway is always
selected, while Search, Debugger, and future optional capabilities can be
checked independently for the Python environment Desktop resolves for the
local Gateway. Search has substantially larger dependencies. Installing Search
or Debugger does not enable those tools automatically; add their import paths
under **Runtime & Tools** when they should be available to new sessions.

**Settings → General → System Tools** manages host command-line utilities
separately from the CrabCode suite. Ripgrep is checked before installation: an
existing `rg` is reused, otherwise Desktop downloads a checksum-pinned official
release binary into the selected Python environment and exposes its scripts
directory to the local Gateway.
`npm run tauri dev` instead uses the configured Python or
the terminal's active Python environment directly so Gateway source and local
editable installs can be debugged.

For a remote Gateway, prefer HTTPS/WSS. An HTTP remote connection requires
explicit acknowledgement in the connection dialog. A browser UI hosted away
from localhost must also be allowed by the Gateway's `--cors` setting.

Tauri writes non-secret UI state to `~/.crabcode/settings_desktop.json`.
Gateway model and tool settings continue to use the normal `settings.json`.
The Models settings section queries the active Gateway for raw named-model
fields, group inheritance, and effective configuration, and can create, edit,
delete, or set the default model in the selected user, project, or local
settings layer.
The Runtime & Tools settings section edits the two Computer Use modes
(`background_app` and `foreground_desktop`), remote file-snapshot behavior,
and the `extra_tools` import-path list in the selected layer. Background mode
is the default and never falls back automatically to foreground control. Disabling file
snapshots does not disable conversation checkpoints; changes apply to new or
reconnected sessions.

On macOS, background pointer events carry a window ID and window-local position.
The position uses the private `CGEventSetWindowLocation` symbol, resolved at runtime;
if it is unavailable, background pointer input returns an error without switching
to foreground control. Compatibility can change with macOS or the target app.
Scroll results confirm dispatch only (`effect_verified: false`); the agent must
check the target area in the observation. Both macOS modes now use pixels with
positive deltas down/right, rather than the old mixed units and signs.

## Build and test

```bash
npm test
npm run build
npm run tauri build
```

The Gateway WebSocket protocol remains version 1.

The opt-in macOS scroll integration test launches two overlapping, isolated
AppKit windows in one process, checks the intended scroll offset and verifies
that the other window, frontmost app and real pointer stay unchanged. It requires
Accessibility permission for the test runner and an idle pointer/focus during
the input check:

```bash
cd src-tauri
cargo test --lib computer_use::tests::macos_background_scroll_targets_one_of_two_windows_without_focus -- --ignored --nocapture
```
