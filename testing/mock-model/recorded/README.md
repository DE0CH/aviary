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

Only OpenRouter's wire form (mock-model's default, `--wire openrouter`) is recorded and byte-checked: `provider`,
`cost` and `cost_details` fields, no `ping` events, and no `signature_delta` (DeepSeek's thinking blocks are
unsigned). mock-model's `--wire anthropic` (or `"wire": "anthropic"` on a script entry) writes Anthropic's native
framing (`ping` after `message_start`, the input usage in `message_start`, a `signature_delta` closing a signed
thinking block, no OpenRouter fields, `toolu_` ids); it follows Anthropic's streaming documentation and
`tests/mock.rs` checks it as a whole stream, but no recording from Anthropic's API stands behind it (the deployment
has no Anthropic key to record with). A scripted thinking `signature` is streamed as a `signature_delta` in either
framing.
