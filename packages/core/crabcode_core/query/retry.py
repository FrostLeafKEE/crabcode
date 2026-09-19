"""Retry state for streamed model responses."""

from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Callable


INITIAL_RETRY_DELAY_SECONDS = 0.2
INITIAL_CONNECTION_RETRY_DELAY_SECONDS = 5.0
MAX_CONNECTION_RETRY_DELAY_SECONDS = 60.0
MAX_STREAM_RETRIES = 100
MAX_REQUEST_RETRIES = 100


@dataclass(frozen=True)
class StreamRetry:
    """One scheduled reconnect attempt."""

    message: str
    error: str
    delay_seconds: float
    retry_count: int
    max_retries: int
    unbounded: bool = False
    transport_fallback: bool = False
    discarded_text_chars: int = 0


class ResponsesStreamRetryState:
    """Track bounded stream retries and unbounded connection retries."""

    def __init__(self) -> None:
        self.retries = 0
        self.connection_retries = 0
        self.connection_retry_delay = INITIAL_CONNECTION_RETRY_DELAY_SECONDS

    @staticmethod
    def _backoff(attempt: int) -> float:
        base = INITIAL_RETRY_DELAY_SECONDS * (2 ** max(attempt - 1, 0))
        return base * random.uniform(0.9, 1.1)

    def schedule(
        self,
        *,
        error: str,
        max_retries: int,
        connection_failed: bool = False,
        retry_after: float | None = None,
        unbounded_connection_retries: bool = True,
        allow_unbounded_connection_retries: bool = True,
        try_transport_fallback: Callable[[], bool] | None = None,
    ) -> StreamRetry | None:
        """Return the next retry, or ``None`` when the error is terminal.

        Eligible connection failures use a separate unbounded 5..60 second
        budget. Other failures consume the bounded stream budget. After that
        budget, a transport fallback gets one chance and resets the counter.
        """
        max_retries = min(max(0, int(max_retries)), MAX_STREAM_RETRIES)
        if (
            unbounded_connection_retries
            and allow_unbounded_connection_retries
            and connection_failed
        ):
            delay = self.connection_retry_delay
            self.connection_retries += 1
            self.connection_retry_delay = min(
                delay * 2,
                MAX_CONNECTION_RETRY_DELAY_SECONDS,
            )
            return StreamRetry(
                message="Reconnecting... waiting for network",
                error=error,
                delay_seconds=delay,
                retry_count=self.connection_retries,
                max_retries=0,
                unbounded=True,
            )

        if self.retries >= max_retries and try_transport_fallback is not None:
            if bool(try_transport_fallback()):
                self.retries = 0
                return StreamRetry(
                    message="Falling back from WebSockets to HTTPS transport.",
                    error=error,
                    delay_seconds=0.0,
                    retry_count=0,
                    max_retries=max_retries,
                    transport_fallback=True,
                )

        if self.retries >= max_retries:
            return None

        self.retries += 1
        delay = (
            max(0.0, float(retry_after))
            if retry_after is not None
            else self._backoff(self.retries)
        )
        return StreamRetry(
            message=f"Reconnecting... {self.retries}/{max_retries}",
            error=error,
            delay_seconds=delay,
            retry_count=self.retries,
            max_retries=max_retries,
        )


def request_retry_backoff(attempt: int) -> float:
    """Return a 200ms exponential HTTP-request retry delay with jitter."""
    bounded_attempt = max(1, int(attempt))
    base = INITIAL_RETRY_DELAY_SECONDS * (2 ** (bounded_attempt - 1))
    return base * random.uniform(0.9, 1.1)
