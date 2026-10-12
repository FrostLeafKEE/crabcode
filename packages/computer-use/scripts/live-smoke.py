#!/usr/bin/env python3
"""Opt-in macOS end-to-end regression, using only a disposable native fixture.

Requires an authorized Crab Computer Use app. The default Gateway route also
needs Gateway Python dependencies and packages/vscode/node_modules:
  python packages/computer-use/scripts/live-smoke.py --bundle <resource-directory>
  python packages/computer-use/scripts/live-smoke.py --client cli
"""

import argparse
import asyncio
import json
import os
from pathlib import Path
import plistlib
import signal
import socket
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[3]
for package in ("core", "gateway", "cli"):
    sys.path.insert(0, str(ROOT / "packages" / package))

async def until(read, predicate, label, seconds=15):
    deadline = asyncio.get_running_loop().time() + seconds
    while asyncio.get_running_loop().time() < deadline:
        value = read()
        if predicate(value):
            return value
        await asyncio.sleep(0.05)
    raise AssertionError(f"Timed out: {label}")


def read_state(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def require(result, **fields):
    for name, value in fields.items():
        if result.get(name) != value:
            # Never dump screenshots, reference trees or endpoint credentials.
            details = {key: result.get(key) for key in ("ok", "error", "error_code", "action_dispatched", "effect_verified")}
            raise AssertionError(f"Expected {name}={value!r}: {details}")


class CliBackend:
    """Drive the actual CLI binding and Core tool, bypassing Gateway entirely."""

    def __init__(self):
        self.sessions = {}

    async def connect(self):
        from types import SimpleNamespace
        from crabcode_cli.computer_use import connect_session
        for host in ("smoke-first", "smoke-second"):
            session = SimpleNamespace(computer_use_mode="background_app",
                                      computer_use_delivery_policy="allow_foreground")
            self.sessions[host] = session
            assert await connect_session(session), "Install the native component first"

    def capabilities(self, host):
        session = self.sessions[host]
        return session.computer_use_backend.capabilities(session.computer_use_host_id)

    async def execute(self, host, *, session_id, agent_id, action, delivery_policy, **kwargs):
        from crabcode_core.tools.computer_use import ComputerUseTool
        from crabcode_core.types.tool import ToolContext
        session = self.sessions[host]
        session.computer_use_delivery_policy = delivery_policy
        context = ToolContext(session=session, session_id=session_id, agent_id=agent_id)
        tool = ComputerUseTool()
        await tool.setup(context)
        assert tool.is_available(context), "CLI did not expose the ComputerUse tool"
        result = await tool.call(action, context)
        parsed = dict(result.data)
        # Core separates image bytes from model JSON. Recombine only in this
        # fixture assertion adapter, so the same native checks can be reused.
        if result.images and isinstance(parsed.get("screenshot"), dict):
            parsed["screenshot"]["data"] = result.images[0]["data"]
        return parsed

    async def release(self, host, **kwargs):
        session = self.sessions[host]
        await session.computer_use_backend.release(session.computer_use_host_id, **kwargs)

    async def close(self):
        for session in self.sessions.values():
            if getattr(session, "computer_use_backend", None):
                await session.computer_use_backend.close()


async def run(bundle, client="gateway"):
    server = serving = None
    if client == "cli":
        broker = CliBackend()
    else:
        from fastapi import FastAPI
        import uvicorn
        from crabcode_gateway.computer_use import ComputerUseBroker
        from crabcode_gateway.routes.computer_use import router
        broker = ComputerUseBroker(timeout_seconds=30)
        app = FastAPI()
        app.state.computer_use_broker = broker
        app.state.sessions = {}
        app.include_router(router)
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
        listener.listen()
        server = uvicorn.Server(uvicorn.Config(app, log_level="error", lifespan="off"))
        serving = asyncio.create_task(server.serve(sockets=[listener]))
    channels = launch = None
    fixture_pid = None
    state_path = None
    report = []
    try:
        if client == "cli":
            await broker.connect()
        else:
            await until(lambda: server.started, bool, "test Gateway startup")
            script = f"""
          import {{ createRequire }} from 'node:module';
          import {{ nodeComputerUseChannel }} from {json.dumps((ROOT / 'packages/computer-use/client/nodeChannel.js').as_uri())};
          const require = createRequire({json.dumps(str(ROOT / 'packages/vscode/package.json'))});
          const WebSocket = require('ws');
          const hosts = ['smoke-first', 'smoke-second'].map(hostId => nodeComputerUseChannel({{
            gatewayUrl: 'ws://127.0.0.1:{port}/ws', bundleDirectory: {json.dumps(str(bundle))},
            hostId, enabled: true, WebSocket, onState() {{}}
          }}));
          await Promise.all(hosts.map(host => host.channel.connect()));
          process.on('SIGTERM', () => {{ hosts.forEach(host => host.channel.dispose()); setTimeout(() => process.exit(0), 200); }});
          process.stdin.resume();
        """
            channels = await asyncio.create_subprocess_exec("node", "--input-type=module", "-e", script, stdin=asyncio.subprocess.PIPE)
            await until(lambda: [broker.is_available(host) for host in ("smoke-first", "smoke-second")], all, "two independent host registrations", 35)
        caps = broker.capabilities("smoke-first")
        assert caps.get("instance_id") == broker.capabilities("smoke-second").get("instance_id"), "clients did not share one executor"
        assert caps.get("input_available") and caps.get("ax_available"), "Grant Accessibility to the installed Crab Computer Use app and refresh"
        assert caps.get("capture_available"), "Grant Screen Recording to the installed Crab Computer Use app and refresh"
        report.append("two clients share one independent native executor")
        with tempfile.TemporaryDirectory(prefix="crabcode-computer-use-smoke-") as temporary:
            directory = Path(temporary)
            fixture = directory / "InputFixture.app"
            binary = fixture / "Contents/MacOS/input-fixture"
            binary.parent.mkdir(parents=True)
            (fixture / "Contents/Info.plist").write_bytes(plistlib.dumps({
                "CFBundleExecutable": "input-fixture", "CFBundleIdentifier": "io.crabcode.input-test",
                "CFBundleName": "CrabCode input fixture", "CFBundleVersion": "1", "CFBundlePackageType": "APPL",
            }))
            compile_process = await asyncio.create_subprocess_exec("xcrun", "swiftc", str(ROOT / "packages/computer-use/core/tests/fixtures/keyboard_host.swift"), "-o", str(binary))
            assert await compile_process.wait() == 0, "fixture compilation failed"
            state_path = directory / "state.json"
            launch = await asyncio.create_subprocess_exec("open", "-g", "-n", "-W", str(fixture), "--args", str(state_path))
            initial = await until(lambda: read_state(state_path), lambda state: state.get("phase") == "ready", "fixture launch")
            fixture_pid = initial["pid"]
            window = str(initial["first"])

            async def execute(action, session="one", host="smoke-first", policy="allow_foreground"):
                return await broker.execute(host, session_id=session, agent_id=None, target_scope="app_window",
                                            delivery_policy=policy, action={"window_id": window, **action})

            shot = await execute({"action": "observe", "observation": "screenshot"})
            require(shot, ok=True)
            assert shot["screenshot"]["data"] and shot["screenshot"]["width"] > 0
            report.append(f"real window screenshot crosses {client} and native IPC")
            observed = await execute({"action": "observe", "observation": "ax"})
            require(observed, ok=True)
            assert "screenshot" not in observed and "preview_screenshot" not in observed
            if client == "gateway":
                assert any(item.get("frame") for item in broker.restorable_previews("smoke-first")), "monitor preview missing"
            tree = observed["accessibility"]
            editor = next(item for item in tree["elements"] if item["role"] == "AXTextArea")
            edit = {"action": "set_value", "snapshot_id": tree["snapshot_id"], "element_id": editor["element_id"], "text": "Shared AX 验证\nsecond line"}
            for session, host in (("two", "smoke-first"), ("one", "smoke-second")):
                rejected = await execute(edit, session=session, host=host)
                require(rejected, action_dispatched=False, error_code="ax_reference_stale")
            changed = await execute(edit)
            require(changed, ok=True, effect_verified=True, verification_method="ax_value")
            state = await until(lambda: read_state(state_path), lambda state: state.get("first_text") == edit["text"], "AX text mutation")
            assert state["second_text"] == "second"
            require(await execute(edit), action_dispatched=False, error_code="ax_reference_stale")
            report.append("AX edit verified; stale, cross-session and cross-client references rejected")
            observed = await execute({"action": "observe", "observation": "ax"}, policy="strict_background")
            tree = observed["accessibility"]
            editor = next(item for item in tree["elements"] if item["role"] == "AXTextArea")
            require(await execute({**edit, "snapshot_id": tree["snapshot_id"], "element_id": editor["element_id"]}, policy="strict_background"),
                    action_dispatched=False, error_code="background_delivery_unsupported")
            report.append("original strict-background policy still rejects unsupported AX mutation")
            clicks = read_state(state_path)["first_clicks"]
            clicked = await execute({"action": "click", "x": 80, "y": 90, "include_screenshot": False})
            require(clicked, action_dispatched=True)
            await until(lambda: read_state(state_path), lambda state: state.get("first_clicks", 0) > clicks, "native mouse delivery")
            require(await execute({"action": "keypress", "keys": ["CMD", "A"], "include_screenshot": False}), ok=True)
            require(await execute({"action": "type", "text": "Shared keyboard 验证", "include_screenshot": False}), ok=True)
            state = await until(lambda: read_state(state_path), lambda state: state.get("first_text") == "Shared keyboard 验证", "native keyboard delivery")
            assert state["second_text"] == "second"
            report.append("real click, shortcut and Unicode typing affect only the chosen fixture window")
            observed = await execute({"action": "observe", "observation": "ax"})
            tree = observed["accessibility"]
            editor = next(item for item in tree["elements"] if item["role"] == "AXTextArea")
            await broker.release("smoke-first", session_id="one", all_agents=True)
            await asyncio.sleep(0.3)
            require(await execute({**edit, "snapshot_id": tree["snapshot_id"], "element_id": editor["element_id"]}),
                    action_dispatched=False, error_code="ax_reference_stale")
            report.append(f"{client} release invalidates native references immediately")
            # Stop only the PID published by the temporary fixture before cleanup.
            os.kill(initial["pid"], signal.SIGTERM)
            await launch.wait()
            launch = None
            fixture_pid = None
        print(json.dumps({"passed": report}, ensure_ascii=False, indent=2))
    finally:
        if launch is not None:
            if fixture_pid:
                try:
                    os.kill(fixture_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            if launch.returncode is None:
                launch.terminate()
                await launch.wait()
        if channels is not None and channels.returncode is None:
            channels.terminate()
            await asyncio.wait_for(channels.wait(), 5)
        if client == "cli":
            await broker.close()
        if server is not None:
            server.should_exit = True
            await serving


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--client", choices=("gateway", "cli"), default="gateway")
    arguments = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This opt-in fixture requires macOS")
    if arguments.client == "gateway" and arguments.bundle is None:
        parser.error("--bundle is required for the Gateway route")
    asyncio.run(run(arguments.bundle.resolve() if arguments.bundle else None, arguments.client))
