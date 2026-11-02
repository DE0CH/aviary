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

## What is not covered

Only OpenRouter's wire form is recorded and byte-checked: `provider`, `cost` and `cost_details` fields, no `ping`
events, and no `signature_delta` (DeepSeek's thinking blocks are unsigned). Anthropic's native Messages API frames a
stream differently (`ping` after `message_start`, a `signature_delta` closing each thinking block, no OpenRouter
fields), and Claude Code against that framing is not exercised here. mock-model does write a `signature_delta`
before `content_block_stop` when a script gives a thinking block a `signature`, in Anthropic's form, but no
recording checks it byte for byte.
