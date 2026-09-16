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
For local connections, Desktop uses the detected Python only to create a
managed virtual environment at `~/.crabcode/desktop/gateway-venv`; CrabCode is
installed and launched there instead of modifying the system, Homebrew, or
Conda environment. `npm run tauri dev` instead uses the configured Python or
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
The Runtime & Tools settings section edits remote file-snapshot behavior and
the `extra_tools` import-path list in the selected layer. Disabling file
snapshots does not disable conversation checkpoints; changes apply to new or
reconnected sessions.

## Build and test

```bash
npm test
npm run build
npm run tauri build
```

The Gateway WebSocket protocol remains version 1.
