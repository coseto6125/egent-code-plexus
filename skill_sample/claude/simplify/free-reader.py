"""
Send a review brief to a free OpenRouter model and write its report.

Usage: python3 free-reader.py <brief-file> <report-file> [context-file]

A context file triggers a second round: the model re-reports its findings
against the full text of the files the diff touches.

Exits non-zero with a one-line reason on stderr when the reader cannot run, so
the caller reports it as a skipped reader instead of a missing report. That
reason never carries the key or the provider's response body: a provider echoes
the request back in some error shapes, and the request is the diff.
"""

from __future__ import annotations

import json
import os
import pathlib
import sys
import urllib.error
import urllib.request

MODEL = "nvidia/nemotron-3-ultra-550b-a55b:free"
ROUND2 = """Here is the full current text of the files this diff touches, plus the callers of the symbols it changes. You said you could not see them.

{context}

Re-report your complete finding list against this context. Correct every failure_scenario this context refutes, drop a finding it disproves, and add any finding it enables. Report the full list, not only what changed."""
ENDPOINT = "https://openrouter.ai/api/v1/chat/completions"
HOST = "openrouter.ai"
STORE = pathlib.Path.home() / ".prime/agent/models.json"
MAX_TOKENS = 16000  # 88% of the budget goes to reasoning tokens; 8000 truncates mid-finding
TIMEOUT_S = 420


def usable(key: object) -> str | None:
    """
    Return the key stripped, or None when it cannot go in a header.

    A stored key with a trailing newline raises ValueError from the header
    machinery, and the exception text carries the whole key.
    """
    if not isinstance(key, str) or not (stripped := key.strip()):
        return None
    return stripped if stripped.isprintable() else None


def api_key() -> str | None:
    """
    Return the OpenRouter key for MODEL, from the environment or the local store.

    The store holds several providers whose keys share a prefix, so the key is
    chosen by matching the provider to the endpoint rather than by scanning
    the file for the first thing that looks like a key. An OpenRouter key
    serves every model, so the store's per-provider model list is not checked.
    """
    if (key := usable(os.environ.get("OPENROUTER_API_KEY"))) is not None:
        return key
    if not STORE.is_file():
        return None
    try:
        providers = json.loads(STORE.read_text()).get("providers", {})
    except (OSError, json.JSONDecodeError, AttributeError):
        return None
    if not isinstance(providers, dict):
        return None
    for provider in providers.values():
        if not isinstance(provider, dict) or HOST not in str(provider.get("baseUrl", "")):
            continue
        if (key := usable(provider.get("apiKey"))) is not None:
            return key
    return None


def post(messages: list[dict], model: str, key: str) -> tuple[dict | None, str | None]:
    """
    Call the completions endpoint. Returns (payload, error-reason).

    The error reason names the failure class only. Provider error bodies quote
    the request, which here is someone's diff.
    """
    body = {
        "model": model,
        "messages": messages,
        "max_tokens": MAX_TOKENS,
        "reasoning": {"effort": "high"},
        "temperature": 0.2,
    }
    request = urllib.request.Request(
        ENDPOINT,
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    try:
        # ENDPOINT is a module constant, https, and never built from an argument.
        payload = json.load(urllib.request.urlopen(request, timeout=TIMEOUT_S))  # ruff: ignore[suspicious-url-open-usage]
    except urllib.error.HTTPError as exc:
        return None, f"HTTP {exc.code}"
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, ValueError) as exc:
        return None, type(exc).__name__
    choices = payload.get("choices") if isinstance(payload, dict) else None
    if not choices:
        # Also the shape a content filter returns: the key is present, the list empty.
        return None, f"no choices ({payload.get('error', {}).get('code', 'unknown')})"
    return payload, None


def content_of(payload: dict) -> str:
    """Return the assistant text of the first choice, empty when the model sent none."""
    message = payload["choices"][0].get("message") or {}
    return message.get("content") or ""


def read_file(path: str, what: str) -> str:
    try:
        return pathlib.Path(path).read_text(encoding="utf-8")
    except OSError as exc:
        sys.exit(f"free reader skipped: cannot read the {what} ({type(exc).__name__})")


def main() -> None:
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    brief = read_file(sys.argv[1], "brief file")
    report = pathlib.Path(sys.argv[2])
    context = read_file(sys.argv[3], "context file") if len(sys.argv) > 3 else None

    key = api_key()
    if key is None:
        sys.exit(f"free reader skipped: no usable OPENROUTER_API_KEY and none for {MODEL} in {STORE}")

    messages = [{"role": "user", "content": brief}]
    payload, error = post(messages, MODEL, key)
    if error is not None:
        sys.exit(f"free reader skipped: {error}")
    rounds, text = 1, content_of(payload)

    if context is not None:
        messages += [
            {"role": "assistant", "content": text},
            {"role": "user", "content": ROUND2.format(context=context)},
        ]
        second, error = post(messages, MODEL, key)
        # Round 2 only ever replaces round 1 with something better. An empty
        # second answer is a failure like any other: keep what round 1 said.
        if error is not None or not (retext := content_of(second)):
            text += f"\n\n---\nround 2 skipped: {error or 'empty response'}\nEvery failure_scenario above is unverified against the enclosing code."
        else:
            payload, rounds, text = second, 2, retext

    if not text:
        sys.exit("free reader skipped: the model returned no content")

    usage = payload.get("usage", {})
    finish = payload["choices"][0].get("finish_reason")
    footer = f"\n\n---\nmodel: {MODEL} · rounds: {rounds} · finish: {finish} · out tokens: {usage.get('completion_tokens')}"
    if finish == "length":
        footer += "\nTRUNCATED — the report stops mid-finding; treat the tail as missing, not as clean."
    try:
        report.write_text(text + footer, encoding="utf-8")
    except OSError as exc:
        sys.exit(f"free reader skipped: cannot write the report ({type(exc).__name__})")
    print(f"free reader wrote {report} (round {rounds}, {finish}, {usage.get('completion_tokens')} out tokens)")


if __name__ == "__main__":
    main()
