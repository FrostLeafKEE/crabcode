"""Usage ledger regressions: provider totals, coverage and calendar boundaries."""

from __future__ import annotations

import asyncio
import base64
from contextlib import closing
import multiprocessing
import sqlite3
import tempfile
import unittest
from datetime import date, datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from crabcode_core.api.base import ModelConfig, StreamChunk, normalize_openai_usage
from crabcode_core.api.anthropic_adapter import _anthropic_usage
from crabcode_core.usage import UsageStore, tracked_stream_message


def _write_usage_rows(path: str, prefix: str) -> None:
    store = UsageStore(Path(path))
    now = int(datetime.now(timezone.utc).timestamp() * 1000)
    for index in range(6):
        request_id = f"{prefix}-{index}"
        store.begin(request_id, now, cwd=None, model="m", provider="p", purpose="chat", session_id=None)
        store.snapshot(request_id, {"total_input_tokens": 3, "output_tokens": 2})
        store.finish(request_id, {"total_input_tokens": 3, "output_tokens": 2}, "completed")


class _Adapter:
    config = type("Config", (), {"provider": "fake"})()

    def __init__(self, chunks):
        self.chunks = chunks

    async def stream_message(self, **_kwargs):
        for chunk in self.chunks:
            yield chunk


class UsageStoreTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.store = UsageStore(Path(self.temp.name) / "usage.sqlite3")

    def test_repeated_provider_snapshots_are_not_summed(self):
        chunks = [StreamChunk(type="text", text="ok", usage={"total_input_tokens": 12, "output_tokens": 4}),
                  StreamChunk(type="message_stop", usage={"total_input_tokens": 12, "output_tokens": 7})]

        async def run():
            with patch("crabcode_core.usage.UsageStore", return_value=self.store):
                return [chunk async for chunk in tracked_stream_message(
                    _Adapter(chunks), messages=[], system=[], tools=[], config=ModelConfig(model="m1"),
                    cwd=self.temp.name, purpose="conversation")]

        self.assertEqual(len(asyncio.run(run())), 2)
        today = datetime.now(timezone.utc).date()
        result = self.store.daily(today, today, ZoneInfo("UTC"), self.temp.name)
        self.assertEqual(result["summary"]["total_tokens"], 19)
        self.assertEqual(result["summary"]["request_count"], 1)
        self.assertEqual(result["summary"]["unknown_requests"], 0)
        self.assertEqual(result["models"][0]["model"], "fake/m1")

    def test_missing_usage_is_partial_not_zero_complete(self):
        request_id = "missing"
        now = int(datetime.now(timezone.utc).timestamp() * 1000)
        self.store.begin(request_id, now, cwd=None, model="m2", provider="fake", purpose="test", session_id=None)
        self.store.finish(request_id, {}, "completed")
        today = datetime.now(timezone.utc).date()
        result = self.store.daily(today, today, ZoneInfo("UTC"))
        self.assertEqual(result["summary"]["unknown_requests"], 1)
        self.assertEqual(result["days"][0]["coverage"], "partial")

    def test_explicit_zero_usage_is_not_missing(self):
        now = int(datetime.now(timezone.utc).timestamp() * 1000)
        self.store.begin("zero", now, cwd=None, model="m", provider="p", purpose="chat", session_id=None)
        self.store.finish("zero", {"input_tokens": 0, "output_tokens": 0}, "completed")
        today = datetime.now(timezone.utc).date()
        result = self.store.daily(today, today, ZoneInfo("UTC"))
        self.assertEqual(result["days"][0]["total_tokens"], 0)
        self.assertEqual(result["summary"]["unknown_requests"], 0)

    def test_model_activity_count_is_all_time_and_project_scoped(self):
        earlier = int(datetime(2026, 9, 25, 12, tzinfo=timezone.utc).timestamp() * 1000)
        later = int(datetime(2026, 9, 26, 12, tzinfo=timezone.utc).timestamp() * 1000)
        for request_id, started, cwd, model in (
            ("first", earlier, self.temp.name, "m"),
            ("second", later, self.temp.name, "m"),
            ("other-project", later, self.temp.name + "-other", "m"),
            ("other-model", later, self.temp.name, "different"),
        ):
            self.store.begin(request_id, started, cwd=cwd, model=model, provider="p",
                             purpose="chat", session_id=None)
            self.store.finish(request_id, {"input_tokens": 0, "output_tokens": 0}, "completed")
        result = self.store.daily(date(2026, 9, 25), date(2026, 9, 25),
                                  ZoneInfo("UTC"), self.temp.name)
        self.assertEqual(result["models"][0]["recorded_request_count"], 2)
        self.assertEqual(result["models"][0]["total_tokens"], 0)
        self.store.begin("new", later, cwd=self.temp.name, model="m", provider="p",
                         purpose="chat", session_id=None)
        self.store.finish("new", {}, "error")
        result = self.store.daily(date(2026, 9, 25), date(2026, 9, 25),
                                  ZoneInfo("UTC"), self.temp.name)
        self.assertEqual(result["models"][0]["recorded_request_count"], 3)
        self.assertEqual(result["models"][0]["total_tokens"], 0)
        from crabcode_gateway.schemas import UsageDailyResponse
        self.assertEqual(UsageDailyResponse.model_validate(result).models[0].recorded_request_count, 3)

    def test_local_day_and_project_filter(self):
        started = int(datetime(2026, 9, 25, 23, 30, tzinfo=timezone.utc).timestamp() * 1000)
        self.store.begin("one", started, cwd=self.temp.name, model="m1", provider="fake", purpose="chat", session_id=None)
        self.store.finish("one", {"total_input_tokens": 2, "output_tokens": 3}, "completed")
        result = self.store.daily(date(2026, 9, 26), date(2026, 9, 26), ZoneInfo("Asia/Shanghai"), self.temp.name)
        self.assertEqual(result["days"][0]["total_tokens"], 5)
        self.assertEqual(self.store.daily(date(2026, 9, 26), date(2026, 9, 26),
                                          ZoneInfo("Asia/Shanghai"), self.temp.name + "-other")["summary"]["total_tokens"], 0)

    def test_interrupted_stream_keeps_reported_partial_usage(self):
        async def run():
            with patch("crabcode_core.usage.UsageStore", return_value=self.store):
                stream = tracked_stream_message(
                    _Adapter([StreamChunk(type="text", text="first", usage={"input_tokens": 5}),
                              StreamChunk(type="text", text="second")]),
                    messages=[], system=[], tools=[], config=ModelConfig(model="m1"))
                await stream.__anext__()
                with closing(sqlite3.connect(self.store.path)) as db:
                    self.assertEqual(db.execute("SELECT status, input_tokens FROM requests").fetchone(),
                                     ("running", 5))
                await stream.aclose()

        asyncio.run(run())
        with closing(sqlite3.connect(self.store.path)) as db:
            self.assertEqual(db.execute("SELECT status, input_tokens, output_tokens FROM requests").fetchone(),
                             ("interrupted", 5, None))

    def test_provider_error_is_rethrown_and_reported_usage_is_kept(self):
        async def failing(**_kwargs):
            yield StreamChunk(type="message_delta", usage={"input_tokens": 4, "output_tokens": 2})
            raise RuntimeError("provider disconnected")

        async def run():
            with patch("crabcode_core.usage.UsageStore", return_value=self.store):
                return [chunk async for chunk in tracked_stream_message(
                    SimpleNamespace(config=None, stream_message=failing), messages=[], system=[],
                    tools=[], config=ModelConfig(model="m"))]

        with self.assertRaisesRegex(RuntimeError, "provider disconnected"):
            asyncio.run(run())
        with closing(sqlite3.connect(self.store.path)) as db:
            self.assertEqual(db.execute("SELECT status, input_tokens, output_tokens FROM requests").fetchone(),
                             ("error", 4, 2))

    def test_two_models_and_total_only_usage(self):
        now = int(datetime.now(timezone.utc).timestamp() * 1000)
        for request_id, model, input_count, output_count in (
            ("a", "A", 100, 20), ("b", "B", 50, 10),
        ):
            self.store.begin(request_id, now, cwd=None, model=model, provider="openai", purpose="chat", session_id=None)
            self.store.finish(request_id, {"total_input_tokens": input_count, "output_tokens": output_count}, "completed")
        self.store.begin("total", now, cwd=None, model="C", provider="openai", purpose="chat", session_id=None)
        self.store.finish("total", {"total_tokens": 13}, "completed")
        today = datetime.now(timezone.utc).date()
        result = self.store.daily(today, today, ZoneInfo("UTC"))
        self.assertEqual(result["summary"]["total_tokens"], 193)
        self.assertEqual(result["summary"]["input_tokens"], 150)
        self.assertEqual(result["summary"]["unknown_requests"], 1)
        self.assertEqual({model["key"]: model["total_tokens"] for model in result["models"]},
                         {"openai/A": 120, "openai/B": 60, "openai/C": 13})

    def test_timezone_dst_and_range_boundaries(self):
        for index, hour in enumerate((6, 7)):
            started = int(datetime(2026, 3, 8, hour, 30, tzinfo=timezone.utc).timestamp() * 1000)
            self.store.begin(str(index), started, cwd=None, model="m", provider="p", purpose="chat", session_id=None)
            self.store.finish(str(index), {"input_tokens": 1, "output_tokens": 1}, "completed")
        result = self.store.daily(date(2026, 3, 8), date(2026, 3, 8), ZoneInfo("America/New_York"))
        self.assertEqual(result["summary"]["total_tokens"], 4)
        self.assertEqual(len(self.store.daily(date(2026, 1, 1), date(2027, 1, 1), ZoneInfo("UTC"))["days"]), 366)
        with self.assertRaises(ValueError):
            self.store.daily(date(2026, 1, 1), date(2027, 1, 2), ZoneInfo("UTC"))

    def test_provider_cache_tokens_are_not_double_counted(self):
        openai = normalize_openai_usage({"prompt_tokens": 110, "completion_tokens": 20,
                                         "prompt_tokens_details": {"cached_tokens": 40},
                                         "completion_tokens_details": {"reasoning_tokens": 4}})
        anthropic = _anthropic_usage({"input_tokens": 70, "cache_read_input_tokens": 40,
                                       "output_tokens": 20}, include_input=True, include_output=True)
        self.assertEqual(openai["total_input_tokens"] + openai["output_tokens"], 130)
        self.assertEqual(openai["reasoning_tokens"], 4)
        self.assertEqual(anthropic["total_input_tokens"] + anthropic["output_tokens"], 130)

    def test_two_processes_do_not_overwrite_each_other(self):
        context = multiprocessing.get_context("spawn")
        workers = [context.Process(target=_write_usage_rows, args=(str(self.store.path), str(index)))
                   for index in range(2)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join(timeout=20)
            self.assertEqual(worker.exitcode, 0)
        today = datetime.now(timezone.utc).date()
        result = self.store.daily(today, today, ZoneInfo("UTC"))
        self.assertEqual(result["summary"]["request_count"], 12)
        self.assertEqual(result["summary"]["total_tokens"], 60)

    def test_daily_api_validates_dates_zone_and_storage_failure(self):
        from fastapi import FastAPI
        from fastapi.testclient import TestClient
        from crabcode_gateway.routes import usage as route

        app = FastAPI()
        app.include_router(route.router)
        today = datetime.now(timezone.utc).date().isoformat()
        with patch.object(route, "UsageStore", return_value=self.store), TestClient(app) as client:
            params = {"start_date": today, "end_date": today, "timezone": "UTC"}
            response = client.get("/usage/daily", params=params)
            self.assertEqual(response.status_code, 200)
            self.assertIsNone(response.json()["days"][0]["total_tokens"])
            self.assertEqual(client.get("/usage/daily", params={**params, "timezone": "Invalid/Zone"}).status_code, 422)
            with patch.object(route, "ZoneInfo", side_effect=ZoneInfoNotFoundError("missing tzdata")):
                missing_database = client.get("/usage/daily", params={**params, "timezone": "Asia/Shanghai"})
            self.assertEqual(missing_database.status_code, 503)
            self.assertIn("tzdata", missing_database.json()["detail"])
            self.assertEqual(client.get("/usage/daily", params={**params, "start_date": "2020-01-01"}).status_code, 422)
            with patch.object(route.usage_module, "recording_error", "usage write failed"):
                failure = client.get("/usage/daily", params=params)
                self.assertEqual(failure.status_code, 503)
            self.store.mark_error()
            with patch.object(route.usage_module, "recording_error", None):
                self.assertEqual(client.get("/usage/daily", params=params).status_code, 503)

    def test_quick_model_probe_does_not_wait_for_late_usage(self):
        from crabcode_core.types.config import ApiConfig
        from crabcode_gateway.routes.config import _probe_model

        async def stream_message(**_kwargs):
            yield StreamChunk(type="text", text="OK")
            raise AssertionError("quick probe should stop at first text")

        adapter = SimpleNamespace(config=ApiConfig(provider="openai", model="m"),
                                  client=None, stream_message=stream_message)
        with patch("crabcode_core.api.create_adapter", return_value=adapter), \
             patch("crabcode_core.usage.UsageStore", return_value=self.store):
            asyncio.run(_probe_model(adapter.config, self.temp.name))
        with closing(sqlite3.connect(self.store.path)) as db:
            self.assertEqual(db.execute("SELECT status, usage_status FROM requests").fetchone(),
                             ("interrupted", "missing"))

    def test_usage_route_uses_gateway_auth_middleware(self):
        from fastapi import FastAPI
        from fastapi.testclient import TestClient
        from crabcode_gateway.middleware import register_middleware
        from crabcode_gateway.routes import usage as route

        app = FastAPI()
        register_middleware(app, password="private-test", security_mode="password",
                            jwt_secret="test-secret-with-at-least-32-characters")
        app.include_router(route.router)
        today = datetime.now(timezone.utc).date().isoformat()
        params = {"start_date": today, "end_date": today, "timezone": "UTC"}
        with patch.object(route, "UsageStore", return_value=self.store), TestClient(app) as client:
            self.assertEqual(client.get("/usage/daily", params=params).status_code, 401)
            credentials = base64.b64encode(b"crabcode:private-test").decode("ascii")
            self.assertEqual(client.get("/usage/daily", params=params,
                                        headers={"Authorization": f"Basic {credentials}"}).status_code, 200)


if __name__ == "__main__":
    unittest.main()
