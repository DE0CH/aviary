# herald

Telegram notification channels made on demand for a conversation. A sandbox asks for a channel; herald creates a
private broadcast channel as its Telegram **account** (MTProto), makes its **bot** an admin that may post, and answers
the channel's handle and invite link. The conversation then posts to the channel through the bot (Bot API). Who joins
a channel is not herald's concern: it hands out the invite link and nothing else.

herald keeps no state. A channel's **handle** is `<chat id>.<signature>`: the Bot API chat id, signed with
HMAC-SHA256 under `HERALD_HANDLE_KEY` for the conversation that created it. Only that conversation can read or post to
the channel; any other conversation gets 404. Changing the key orphans every handle (the channels stay, posting to
them stops).

## API

The caller is the sandbox gateway (pair token `HERALD_ACCEPT_GATEWAY`), which sets the verified
`Aviary-Sandbox-Conversation`. Bodies are JSON; errors are aviary's envelope (`code`, `message`).

| Request | Answer |
|---|---|
| `POST /v1/channels` `{"title": "…", "description": "…"}` | 201 `{"channel", "title", "inviteLink"}`. The title gets `HERALD_TITLE_PREFIX` and a space in front; at most 128 characters in all; the description at most 255. |
| `GET /v1/channels/{channel}` | 200 `{"channel", "title", "inviteLink"}` as the bot sees the channel now |
| `POST /v1/channels/{channel}/messages` `{"text": "…", "format": "plain"\|"html", "silent": false}` | 201 `{"messageId"}`. At most 4096 characters; `html` is Telegram's HTML subset; `silent` posts without a notification sound. Link previews are off. |

404 = not this conversation's channel, or the channel was deleted / the bot removed from it. 429 with `Retry-After`
= Telegram's flood wait (channel creation is limited per account). 400 = the request, or Telegram refused it (the
message names Telegram's reason). 503 = Telegram unreachable.

## Configuration

| Variable | |
|---|---|
| `HERALD_ACCEPT_GATEWAY` | the gateway's pair token |
| `HERALD_HANDLE_KEY` | at least 32 characters; signs the handles |
| `HERALD_TELEGRAM_API_ID` | the account's app (my.telegram.org) |
| `HERALD_TELEGRAM_SESSION` | the account's session from `herald login` (JSON with the authorization keys: a secret) |
| `HERALD_BOT_TOKEN` | the bot's token (BotFather); herald looks the bot up with `getMe` at start |
| `HERALD_SOCKS` | `socks5://host:port`: every Telegram connection (MTProto and the Bot API, names resolved by the proxy) goes through it |
| `HERALD_TITLE_PREFIX` | put before every channel title |
| `POD_NAME` | as every aviary service |

Missing or unusable configuration ends the start with exit 78. A session that is no longer signed in also exits 78
(`herald login` again); Telegram unreachable at start is retried every 10 s, `/readyz` 503 (`telegram`) meanwhile.

## Signing the account in

```
HERALD_TELEGRAM_API_ID=… HERALD_TELEGRAM_API_HASH=… HERALD_SOCKS=socks5://… herald login +441234567890 > session.json
```

It asks Telegram for a login code (sent to the account's other sessions, e.g. the phone app), reads the code and then
the 2FA password, if the account has one, from stdin, and writes the session to stdout. The session holds only the
DC options and their permanent keys (`src/session.rs`).
