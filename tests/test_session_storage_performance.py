"""Regression tests for bounded session persistence work."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from crabcode_core.session.meta_db import SessionMetaStore, reset_shared_connections
from crabcode_core.session.storage import SessionStorage, get_callback_deliveries_path
from crabcode_core.types.message import create_user_message


@pytest.fixture(autouse=True)
def close_shared_metadata_connections():
    reset_shared_connections()
    yield
    reset_shared_connections()


def test_metadata_updates_scan_only_new_transcript_tail(tmp_path, monkeypatch):
    monkeypatch.setattr("crabcode_core.session.storage.get_config_home", lambda: tmp_path / "config")
    monkeypatch.setattr("crabcode_core.session.meta_db.get_config_home", lambda: tmp_path / "config")
    storage = SessionStorage(str(tmp_path), "incremental-meta")
    storage.write_meta(model="initial")
    storage.append_message(create_user_message("large prefix " * 20_000))
    storage.load_messages()

    storage.record_tokens(7)
    scanned_after_first = storage._scan_offset
    storage.record_message_count(1)

    assert storage._scan_offset > scanned_after_first
    assert storage.meta["tokens_used"] == 7
    assert storage.meta["message_count"] == 1
    assert storage._scan_offset == storage._transcript_path.stat().st_size


def test_append_message_deduplicates_across_storage_instances(tmp_path, monkeypatch):
    monkeypatch.setattr("crabcode_core.session.storage.get_config_home", lambda: tmp_path / "config")
    first = SessionStorage(str(tmp_path), "dedupe")
    second = SessionStorage(str(tmp_path), "dedupe")
    message = create_user_message("same message")

    first.append_message(message)
    second.append_message(message)

    rows = [json.loads(line) for line in first._transcript_path.read_text().splitlines()]
    assert sum(row.get("uuid") == message.uuid for row in rows) == 1


def test_callback_deliveries_use_small_sidecar_and_remain_idempotent(tmp_path, monkeypatch):
    monkeypatch.setattr("crabcode_core.session.storage.get_config_home", lambda: tmp_path / "config")
    storage = SessionStorage(str(tmp_path), "callbacks")
    kwargs = {
        "agent_id": "agent",
        "callback_epoch": 3,
        "callback_message_id": "message",
        "assistant_uuid": "assistant",
    }

    assert storage.record_callback_delivery(**kwargs)
    assert storage.record_callback_delivery(**kwargs)

    path = get_callback_deliveries_path(str(tmp_path), "callbacks")
    assert len(path.read_text().splitlines()) == 1
    assert not storage._transcript_path.exists()
    restored = SessionStorage(str(tmp_path), "callbacks")
    assert restored.has_callback_delivery(
        agent_id="agent",
        callback_epoch=3,
        callback_message_id="message",
    )


def test_legacy_callback_delivery_still_loads_from_main_transcript(tmp_path, monkeypatch):
    monkeypatch.setattr("crabcode_core.session.storage.get_config_home", lambda: tmp_path / "config")
    storage = SessionStorage(str(tmp_path), "legacy-callback")
    storage._append_transcript_line(
        {
            "type": "callback_delivery",
            "session_id": storage.session_id,
            "agent_id": "old-agent",
            "callback_epoch": 2,
            "callback_message_id": "old-message",
            "assistant_uuid": "old-assistant",
        }
    )

    restored = SessionStorage(str(tmp_path), storage.session_id)
    assert restored.has_callback_delivery(
        agent_id="old-agent",
        callback_epoch=2,
        callback_message_id="old-message",
    )
    sidecar = get_callback_deliveries_path(str(tmp_path), storage.session_id)
    assert sidecar.exists()
    assert len(sidecar.read_text().splitlines()) == 1


def test_metadata_store_reuses_connection_on_same_thread(tmp_path):
    path = Path(tmp_path) / "sessions.db"
    first = SessionMetaStore(path)
    second = SessionMetaStore(path)

    first.get("missing")
    second.get("missing")

    assert first._conn is second._conn
