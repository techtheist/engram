"""Shared retry helper with exponential backoff."""

import time


class RetryExhausted(Exception):
    """Raised when every attempt failed."""


def with_retry(fn, attempts=3, base_delay=0.05, max_delay=2.0):
    """Call fn() until it returns; back off exponentially between failures."""
    delay = base_delay
    last = None
    for attempt in range(1, attempts + 1):
        try:
            return fn()
        except Exception as exc:  # noqa: BLE001 - any failure is retryable here
            last = exc
            if attempt == attempts:
                break
            time.sleep(delay)
            delay = min(delay * 2, max_delay)
    raise RetryExhausted(f"gave up after {attempts} attempts: {last!r}") from last
