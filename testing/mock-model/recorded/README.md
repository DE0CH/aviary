# Recorded responses

Real streaming answers of the Messages API that YYC's gateway relays to (OpenRouter's Anthropic-compatible
`POST https://openrouter.ai/api/v1/messages`, model `deepseek/deepseek-v4.1-flash-1017`, `anthropic-version:
2023-06-01`), recorded on 2026-11-02 with `curl -N` and stored byte for byte (response body only; no request or
response headers, no credentials).

| File | Request |
|---|---|
| `openrouter-thinking-text.sse` | `max_tokens: 60`, one user message `Reply with exactly: Hello from the recording.` |
| `openrouter-thinking-tool-use.sse` | `max_tokens: 200`, a `Bash` tool (`{"command": string}`), `tool_choice: {"type": "tool", "name": "Bash"}`, one user message `Use the Bash tool to run: echo hi > /workspace/a.txt` |

`tests/mock.rs` scripts mock-model with each recording's content, ids and usage and asserts that its stream is
byte-identical, so mock-model's framing, event order and JSON field order are the upstream's. To re-record (for a
new upstream wire form), repeat the request, replace the file and update the script in the test.
