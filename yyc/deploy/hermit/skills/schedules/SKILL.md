---
name: schedules
description: Arm, list, change and cancel scheduled messages in this conversation (a reminder at a time, or a repeating task such as every weekday at 8:30), through the schedules API. Use it whenever someone asks you to do something later, at a time, or regularly, or asks about, changes or cancels a schedule, and when a <scheduled-message> arrives.
---

# Schedules

Staff ask you to do something later, or again and again: "remind me on Friday at 9 to follow up", "every weekday at
8:30, collect yesterday's news about X with sources". You arm a schedule: at that time the exact message you set is
posted into this conversation, marked as a scheduled message, and you answer it then like any message. A schedule
always posts into the conversation it was set in, for the staff member you are talking to.

Staff see each schedule in this conversation: a card in the chat when you set one, and a 「n 個排程」 chip at the top
that opens a panel with every schedule's rule, next time, message and recent runs. They change or cancel one by
telling you; you make the change with the API below.

## The API

```sh
S() { curl -sS "$@"; }   # define in each command
B=http://magpie/v1/sandbox/schedules
```

A refused request answers JSON with `code` and a `message` saying what is wrong.

```sh
S "$B"                                        # every schedule of this person (all their conversations)
S "$B/<id>"                                   # one, with its last runs
S -X POST "$B" -H 'Content-Type: application/json' \
  -d '{"prompt": "提醒我把新聞稿的定稿發給 Ming。", "at": "2026-11-09T15:00+08:00"}'                 # once
S -X POST "$B" -H 'Content-Type: application/json' \
  -d '{"prompt": "整理昨天關於林定國的新聞，列出來源。", "cron": "30 8 * * 1-5", "tz": "Asia/Hong_Kong"}'   # repeating
S -X PATCH "$B/<id>" -H 'Content-Type: application/json' -d '{"paused": true}'      # pause; false resumes
S -X PATCH "$B/<id>" -H 'Content-Type: application/json' -d '{"prompt": "…"}'       # a new message
S -X PATCH "$B/<id>" -H 'Content-Type: application/json' -d '{"cron": "0 9 * * 1-5"}' # a new rule (or "at", "tz", "until")
S -X DELETE "$B/<id>"                                                                 # cancel it
```

- `prompt`: the exact message posted at that time, written as the staff member would type it to you. It is all you
  will have then, so make it complete (what to do, about whom, where the result goes), not "do what we discussed".
- `at`: one time, with its offset (`2026-11-07T09:00+08:00`). `cron`: five fields, minute hour day-of-month month
  day-of-week (`30 8 * * 1-5` = weekdays at 08:30), read in `tz` (default `Asia/Hong_Kong`); `until` (optional) ends
  it. Use the staff member's time zone; it is Hong Kong unless they say otherwise.
- A repeating schedule fires at most every 15 minutes, and a person has at most 25 schedules; a request past either is
  refused with a message saying so.

Each answer is the schedule: `id`, `describe` (the rule as the server read it, in words, e.g. `每個工作日 08:30`),
`next` (the next time), `state` (`active`, `paused`, `done`) and, for one schedule, `runs` (each due time and what
happened: `sent`, `sent_mid_turn`, `skipped_late`, …).

After arming or changing one, tell the staff member the server's `describe` and `next` in your own sentence, so they
can see it is what they meant; if it is not, fix it at once. Call the API with plain `curl` as above (no `jq` or other
filters), so the chat shows the answer.

## When it fires

The message arrives as `<scheduled-message rule="每個工作日 08:30">…</scheduled-message>`: a scheduled message the
staff member set earlier, not something they just typed. Do what it says and answer as you would to them; they read it
later. If it arrives while you are still working on something else, finish or pause that sensibly and answer it too.

Schedules are this API's alone: the built-in cron and wake-up tools are not available here.
