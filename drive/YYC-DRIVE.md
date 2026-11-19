# YYC Drive

One shared folder for a whole company, opened in a browser. This repository is a fork of
[opencloud-eu/web](https://github.com/opencloud-eu/web) (AGPL-3.0): the page is OpenCloud's, cut down; the edge
(`edge/`, a Cloudflare Worker) serves it and answers its settings, and `server/`, a small program of its own, answers
the folder's calls. OpenCloud's server is not used.

## What the fork changes in the page

- The drive is shown under the name the edge gives it, not "Personal" (`web-app-files/src/index.ts`,
  `web-runtime/src/index.ts`, `web-pkg/src/composables/folderLink/useFolderLink.ts`,
  `web-runtime/src/components/UploadInfo.vue`).
- The top bar has nothing on the right: no sidebar toggle, app menu or user menu
  (`web-runtime/src/components/Topbar/TopBar.vue`).

Everything else that is gone (Favorites, Shares, Spaces, Deleted files, notifications, the feedback link, the support
card) is gone because the edge says it does not offer it (its capabilities and `config.json`, `edge/src/settings.js`).

## The server (`server/`, Rust)

One program and one folder. No database, no per-person folders, no sharing. The page, its settings and its sign-in
are the edge's (below); the server answers only the folder's calls.

- Sign-in belongs to Cloudflare Access in front; the edge names the person in a request header, and a request without
  one is refused.
- WebDAV on the folder: list, read (byte ranges, so video seeks), write, new folder, rename, move, copy, delete.
- Uploads in 10 MB pieces (tus), kept in `<folder>/.drive-uploads` until the last piece is in.
- Search by name.
- Previews of videos and pictures: one frame (a second in) as a JPEG the size the page asks, drawn by ffmpeg (in the
  image) two at a time, cached by the browser under the file's etag; none for anything else (the page shows the icon).
- Names that start with a dot are hidden and refused; a path cannot leave the folder.
- A request's body is read to its end before the answer (at most 1 MiB), except an upload's, which is streamed to disk:
  answered with its body still arriving, a connection is closed after the answer, and the proxy in front may already
  be sending its next request there (cloudflared answered that request 502; on the page's sign-in POSTs that was
  "access denied", about one fresh load in 140).

Each setting is a flag or, without it, an environment variable:

| Flag | Variable | Meaning |
|---|---|---|
| `--root` | `DRIVE_ROOT` | the folder to serve |
| `--name` | `DRIVE_NAME` | the name of the folder's top (default `Drive`; the edge's `DRIVE_NAME` is the same) |
| `--listen` | `DRIVE_LISTEN` | address to listen on (default `0.0.0.0:8080`) |
| `--user-header` | `DRIVE_USER_HEADER` | the header that names the signed-in person (default `cf-access-authenticated-user-email`, which the edge sets) |
| `--dev-user` | `DRIVE_DEV_USER` | a person to assume when the header is absent: local runs only |
| `--url` | `DRIVE_URL` | the public address, when it cannot be read from the request |

`GET /healthz` answers 200 while the program runs; `GET /readyz` answers 200 while the folder is there. On SIGTERM
it takes no new connections, lets the transfers in flight finish, and exits 0.

## The edge (`edge/`, a Cloudflare Worker)

The page loads from Cloudflare's edge, near whoever opens it, and only the folder's own calls travel to the server.
Before the page shows a folder it asks about twenty questions in a row (its files, its settings, its sign-in, who is
signed in, which drive there is); from Hong Kong to a server in Germany each was a quarter of a second.

- The built page is the Worker's static assets (`assets/`: `dist/` with the theme at `themes/opencloud/`, and
  `_headers`, which keeps the hashed files for good). Cloudflare serves them before the Worker runs.
- The Worker answers the page's settings and sign-in calls itself (`src/settings.js`) for the person in Access's token
  (`src/access.js`: signed by the team's keys, for the drive's Access app). Without a good token it answers 403.
- The folder's calls (`/dav/`, `/data/`) go on to the server, with the person's email in the header the server reads
  (`cf-access-authenticated-user-email`, set from the token, never a value the browser sent) and Access's token for
  the tunnel's own check.
- A preview asked for by the file's etag (`?preview=1&c=…`) is one picture for that version of the file, so the
  edge keeps it in its cache for whoever asks next; the browser gets its own private copy.

Its settings are vars: `ACCESS_TEAM`, `ACCESS_AUD` (the Access app's audience tag) and `DRIVE_NAME` (the same name
as the server's `--name`). The image carries the edge at `/srv/edge`; YYC's deploy takes it out of the image it pins,
so the page at the edge and the server are always one commit. Tests: `cd edge && npm test`.

## Run it locally

```sh
pnpm install && pnpm build && mkdir -p dist/themes && cp -r server/theme dist/themes/opencloud
(cd server && cargo build --release)
DRIVE_ROOT=/some/folder DRIVE_NAME="YYC Drive" DRIVE_DEV_USER=me@example.com \
  DRIVE_LISTEN=127.0.0.1:8201 server/target/release/yyc-drive &
mkdir -p edge/assets && cp -r dist/. edge/assets/ && cp edge/_headers edge/assets/
cd edge && npx wrangler dev --var DEV_USER:me@example.com --var ORIGIN:http://127.0.0.1:8201 --var "DRIVE_NAME:YYC Drive"
```

The page is then at http://127.0.0.1:8787. `DEV_USER` is the person a local run assumes; it is taken only when
`ACCESS_AUD` is not set, so a deployed Worker never has one.

## Following upstream

Merge `opencloud-eu/web` `main` into this repository's `main`. The page's calls to the server were recorded from
OpenCloud 9.1.0 with web 8.2.2; a newer page may ask for something the edge or `server/` does not answer yet (both
log every request they turn down: the Worker's logs, the pods' output).
