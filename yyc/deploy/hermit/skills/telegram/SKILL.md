---
name: telegram
description: Create a Telegram channel for this conversation and post messages and files to it, so staff get notifications on Telegram (results of a scheduled task, an alert when something happens, a summary when work finishes, a report or chart as a file). Use it whenever someone asks to be notified, alerted or sent something on Telegram, asks for a Telegram channel, or a scheduled message says to post to one.
---

# Telegram channels

Staff can get messages from this conversation on Telegram. You create a private Telegram channel and give the staff
member its invite link; whoever opens the link joins the channel and sees every message you post there. Messages come
from the YYC AI bot. Typical use: a schedule (schedules skill) does its work and posts the result to the channel, so
it reaches the staff member's phone.

## The API

```sh
T() { curl -sS "$@"; }   # define in each command
B=http://telegram/v1/channels
```

A refused request answers JSON with `code` and a `message` saying what is wrong.

```sh
T -X POST "$B" -H 'Content-Type: application/json' \
  -d '{"title": "每日新聞摘要", "description": "每個工作日 08:30 的新聞摘要"}'          # create
T "$B/<channel>"                                                                      # title and invite link
T -X POST "$B/<channel>/messages" -H 'Content-Type: application/json' \
  -d '{"text": "今日摘要：……"}'                                                         # post
T -X POST "$B/<channel>/messages" -H 'Content-Type: application/json' \
  -d '{"text": "<b>今日摘要</b>\n<a href=\"https://example.com\">來源</a>", "format": "html"}'
T -X POST "$B/<channel>/files" -F 'caption=今日報告' -F file=@/workspace/report.pdf                # a file
T -X POST "$B/<channel>/files" -F as=photo -F 'caption=本週走勢' -F file=@/workspace/chart.png      # an image shown in the channel
```

- **Create** answers `channel` (its handle: keep it), `title` and `inviteLink`. The title gets 「YYC AI ·」 in front;
  keep it short and clear, in the staff member's language. Give the staff member the invite link in your reply and
  say that opening it on their phone joins the channel.
- **Write the handle down** at once in `/workspace/telegram-channels.md` (the handle, the title, what it is for), so
  later turns and scheduled messages find it. A channel belongs to this conversation: other conversations cannot post
  to it, and there is no list of channels to look it up again.
- **Post:** `text` up to 4096 characters (split anything longer into several messages); `format` `"html"` allows
  Telegram's tags (`<b>`, `<i>`, `<u>`, `<s>`, `<a href>`, `<code>`, `<pre>`, `<blockquote>`; escape `<`, `>`, `&` in
  plain text), otherwise the text is shown as written. `"silent": true` posts without a notification sound. Link
  previews are off.
- **Files:** one file per request, as the part `file` (`-F file=@path`), and every other part before it: `caption`
  (up to 1024 characters; longer text goes in a message of its own), `format` (`html` for the caption), `silent`,
  `as`. By default the file is sent as a document, kept exactly as it is, up to 50 MB (PDF, Excel, Word, ZIP, any
  type); `as=photo` shows an image in the channel itself, up to 10 MB, recompressed by Telegram: use it for charts and
  screenshots people should see at a glance. The file name shown is the file's own: name the file well before sending
  (e.g. `/workspace/每日新聞摘要-2026-11-11.pdf`). A larger file: 413; split it or make it smaller.
- Make one channel per purpose and reuse it; create a new one only when the staff member asks for a separate channel.
  Who is in a channel is up to whoever has the link: you cannot add or remove people.
- 404 means the handle is not this conversation's, or the channel was deleted. 429 means Telegram asks to wait (the
  `Retry-After` header, in seconds): wait that long and try once more. 503: Telegram could not be reached; try again a
  little later.

## With schedules

When a schedule should notify someone on Telegram, create the channel first, then write the handle into the
schedule's `prompt`, for example: 「整理昨天關於林定國的新聞，列出來源，然後把摘要發到 Telegram 頻道
`-1001234567890.AbCd…`（telegram 技能）。」 When the scheduled message arrives, do the work and post the result to that
channel (a message, or the file it produced); in this conversation, a short note that it was posted is enough.

Call the API with plain `curl` as above (no `jq` or other filters), so the chat shows the answer.
