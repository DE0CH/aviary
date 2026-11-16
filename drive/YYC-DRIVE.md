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
- Names that start with a dot are hidden and refused; a path cannot leave the folder.

| Variable | Meaning |
|---|---|
| `DRIVE_ROOT` | the folder to serve |
| `DRIVE_WEB` | the built page (`dist/`, with the theme copied to `dist/themes/opencloud/`) |
| `DRIVE_NAME` | the drive's name in the page (default `Drive`) |
| `DRIVE_LISTEN` | address to listen on (default `0.0.0.0:8080`) |
| `DRIVE_USER_HEADER` | the header that names the signed-in person (default `cf-access-authenticated-user-email`) |
| `DRIVE_DEV_USER` | a person to assume when the header is absent: local runs only |
| `DRIVE_URL` | the public address, when it cannot be read from the request |

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
