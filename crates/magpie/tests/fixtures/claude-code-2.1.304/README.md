# Claude Code 2.1.304 transcripts

The final files of the FUSE gate's scripted session (WP-07 gate item 2: `fabric/fuse-gate/results/hetzner/st2`,
`.claude/projects/-home-sandbox-work/`): the main transcript after a relocation with an occupied destination, a
compaction and continued turns (`main.jsonl`, with a torn line Claude Code left unparsable), and its subagent's
transcript and meta file (`agent.jsonl`, `agent.meta.json`). Written by the real binary against a mock Messages
API. Two edits, both keeping every byte offset, line length and the JSON structure: the guest kernel's name in the
environment block is replaced with a word of the same length, and Claude Code's own product text is replaced with
`placeholder text` of the same byte length (the system prompt and tool descriptions of the `prompt_snapshot`
attachments, and every prose string of the internal records: attachments, `api-request-shape`, `api-request-blob`,
including the torn line). The replay needs only the lengths and the structure.

`tests/ccfs_claude_code.rs` replays them through magpie's cc routes in the shape of the gate's recorded calls.
