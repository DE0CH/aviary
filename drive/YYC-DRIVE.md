# YYC Drive

One shared folder for a whole company, opened in a browser. This repository is a fork of
[opencloud-eu/web](https://github.com/opencloud-eu/web) (AGPL-3.0): the page is OpenCloud's, cut down, and the
server is `server/`, a small program of its own. OpenCloud's server is not used.

## What the fork changes in the page

- The drive is shown under the name the server gives it, not "Personal" (`web-app-files/src/index.ts`,
  `web-runtime/src/index.ts`, `web-pkg/src/composables/folderLink/useFolderLink.ts`,
  `web-runtime/src/components/UploadInfo.vue`).
- The top bar has nothing on the right: no sidebar toggle, app menu or user menu
  (`web-runtime/src/components/Topbar/TopBar.vue`).

Everything else that is gone (Favorites, Shares, Spaces, Deleted files, notifications, the feedback link, the support
card) is gone because `server/` says it does not offer it (its capabilities and `config.json`).

## The server (`server/`, Rust)

One program and one folder. No database, no per-person folders, no sharing.

- Serves the built page (`dist/`), `config.json` and the theme (`server/theme`, served at `/themes/opencloud/`).
- Sign-in belongs to the proxy in front (Cloudflare Access), which names the person in a request header. The page's
  own OIDC step is answered at once by `/oidc/*`, so nobody sees a second login.
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
| `--web` | `DRIVE_WEB` | the built page (`dist/`, with the theme copied to `dist/themes/opencloud/`) |
| `--name` | `DRIVE_NAME` | the drive's name in the page (default `Drive`) |
| `--listen` | `DRIVE_LISTEN` | address to listen on (default `0.0.0.0:8080`) |
| `--user-header` | `DRIVE_USER_HEADER` | the header that names the signed-in person (default `cf-access-authenticated-user-email`) |
| `--dev-user` | `DRIVE_DEV_USER` | a person to assume when the header is absent: local runs only |
| `--url` | `DRIVE_URL` | the public address, when it cannot be read from the request |

`GET /healthz` answers 200 while the program runs; `GET /readyz` answers 200 while the folder is there. On SIGTERM
it takes no new connections, lets the transfers in flight finish, and exits 0.

## Run it locally

```sh
pnpm install && pnpm build && mkdir -p dist/themes && cp -r server/theme dist/themes/opencloud
cd server && cargo build --release
DRIVE_ROOT=/some/folder DRIVE_WEB=../dist DRIVE_NAME="YYC Drive" DRIVE_DEV_USER=me@example.com \
  DRIVE_LISTEN=127.0.0.1:8201 ./target/release/yyc-drive
```

## Following upstream

Merge `opencloud-eu/web` `main` into this repository's `main`. The page's calls to the server were recorded from
OpenCloud 9.1.0 with web 8.2.2; a newer page may ask for something `server/` does not answer yet (it logs every
request it turns down).
