# CrabCode Computer Use

Local companion for the CrabCode extension in Remote SSH, WSL and dev-container
windows. Install this extension on the **local** machine and CrabCode in the
workspace extension host. Local CrabCode windows include the same component
and do not require the companion to be active.

Enable `crabcode.computerUseEnabled` in CrabCode settings. The status-bar
Computer Use menu provides enable/disable, permission refresh, monitoring
screenshots and operation history. Operations target the local desktop.

On macOS, grant **Crab Computer Use** Accessibility and Screen Recording
permissions. Crab Desktop is not required. Delivery policy remains controlled
by the existing CrabCode session settings.

Build the platform-specific package with `npm ci`, then
`npm run vscode:prepublish`. See `../../computer-use/README.md` in the source
repository for native development, signing and regression validation.
