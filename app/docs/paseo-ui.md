# The Paseo UI

The browser UI of this fork (YYC AI, and a Jarvis 4 session's `cloudcli` harness) and the YYC AI iOS app are the Paseo app
(getpaseo/paseo, Apache-2.0), copied into `paseo/` at upstream commit e3bbed3 and changed into a chat app. It is not
kept in sync with upstream (Deyao, 2026-10-26): change it as this fork's own code. CloudCLI's server and state stay
as they were; the app reaches them through a bridge that speaks Paseo's protocol.

```
browser (paseo/packages/app, served from dist/)
   │  /paseo-ws: Paseo's WebSocket protocol (paseo/packages/protocol messages.ts)
   │  /api/...:  CloudCLI REST for what the protocol has no message for (uploads, files, sharing, warming, /me)
   ▼
CloudCLI server, one process per person (unchanged)
   server/modules/paseo/paseo-connection.ts   one per browser socket: answers the protocol
   server/modules/paseo/paseo-hub.ts          one per process: a CloudCLI chat client of its own (handleChatConnection),
                                              the conversations' timelines, run status, list changes
   server/modules/paseo/paseo-timeline.ts     CloudCLI messages → Paseo timeline rows (seq, epoch, pages)
   └─ sessionsService, chatRunRegistry, the provider runtime, Postgres, the sandbox (as before)
```

The Jarvis 4 Chat app (DE0CH/pwa-shell-ios) keeps talking CloudCLI's own `/ws` and REST; the bridge adds a path, it
does not change one.

## How the app sees CloudCLI

| Paseo | CloudCLI |
|---|---|
| host (`srv_conversations`, named after the product) | the per-person server process |
| one project (`prj_conversations`, the workspace path) | the one project of Jarvis mode |
| workspace `wks_<session id>` | a conversation (session row) |
| agent `<session id>`, provider `claude`, one model | the same conversation; the instance's model |
| workspace title | the session name (`workspace.title.set` = rename) |
| archive (workspace or agent) | delete = Recently deleted |
| History page, archived agents | Recently deleted; opening one + Unarchive (`refresh_agent_request`) = restore |
| `delete_agent_request` on an archived agent | delete for good |
| timeline rows | the session's history, then the run's live events |
| `send_agent_message_request`, `workspace.create.request` | `chat.send` (a new session first for a create) |
| `cancel_agent_request` | `chat.abort` |
| `yyc.sandbox_status` (added to the protocol) | the run's `status` frames with `jarvisSandbox` |

Everything a developer machine has (git, terminals, scripts, plugins, worktrees, providers, voice) answers empty or
`rpc_error`, and the server_info features leave it out. Add a feature by answering its messages in
`paseo-connection.ts` and checking the replies against `WSOutboundMessageSchema` (below): the app validates every
frame and drops one that does not parse, usually with "Response validation failed for …".

Timelines: the first fetch of a conversation reads its whole history (sessionsService.fetchHistory) into rows; live
events from the hub's chat socket are appended after that, deduplicated by message id. The user's own message goes on
the timeline when it is sent (CloudCLI's live stream does not echo it) with the app's `clientMessageId`, which is how
the app replaces its optimistic copy. Epochs change with the process, so a restart makes every open tab fetch the tail
again. A run started elsewhere (another tab, the iOS app, a scheduled turn) is followed when its `session_upserted`
arrives.

## What changed in the app (paseo/packages/app)

- **Connection**: the page connects back to its own origin (`runtime/host-runtime.ts`
  `readPageOriginConnectionHint`) on `/paseo-ws` (`protocol/src/daemon-endpoints.ts`); no host setup, no pairing. When
  the server cannot be reached, `app/welcome.tsx` says so and offers a retry.
- **Signed out (web)**: only `/api`, `/paseo-ws`, `/ws`, `/ops` (and `/__jarvis`) are behind Cloudflare Access; the
  router (`jarvis/router.mjs`, "Signed out") serves a visitor without a valid Access token the built app from `dist/`
  and nothing else, its `index.html` marked `<meta name="yyc-signed-out" content="1">`. `yyc/access-gate.tsx` then
  renders `yyc/signed-out-shell.tsx` instead of the app: the new-chat layout with the composer disabled and 「登入」
  buttons, with no host runtime, socket or API call. 「登入」 opens `/api/jarvis/signin?next=<page>` (behind Access:
  Access's login, then the router's 302 back to the page, now signed in). The Access app's paths, cloudflared's
  access-required rule and the router's `SIGNED_IN_PATHS` are one list (DE0CH/YYC tofu `access.tf` `staff_ai_paths`).
- **Language**: zh-HK (generated from Paseo's zh-CN with OpenCC, HK vocabulary) and English only; any Chinese or
  unknown system locale gets zh-HK. YYC's own wording is an override layer, `i18n/resources/yyc.ts`; new strings of
  this fork live under `conversation.*` in `en.ts`/`zh-HK.ts`.
- **Layout**: a flat list of conversations (no project row, `components/sidebar-workspace-list.tsx`), home is a new
  chat (`utils/host-routes.ts` `buildOpenProjectRoute`), no tab row (`WorkspaceDesktopTabsRow` renders nothing), no
  project/host/mode pickers, explorer, editor, scripts, hover card or fork button; the composer has no voice,
  autocomplete or model/mode chips (`composer/input-mode.ts`) and picks the one provider and model by itself
  (`provider-selection/resolve-agent-form.ts`); where the model chip was, it shows the model's name read-only with an
  (i) saying the admin sets it (`yyc/model-label.tsx`, from config `models`' label for `defaultModel`).
- **Developer controls hidden** (staff are not developers; each is a constant or a server_info feature, so the
  upstream code path stays): the composer's 「新增 issue 或 PR」 GitHub picker (only with `features.forgeSearch`, which
  CloudCLI never sends); the fork menu on earlier replies; the explorer (`workspace-tabs/explorer-sidebar.ts`
  `EXPLORER_ENABLED`: no swipe, shortcut or stored layout opens it); every key-chord hint, including the composer's
  「⌘L 聚焦」 and the Alt number badges (`components/ui/shortcut.tsx` `SHOW_SHORTCUT_HINTS`); the key handler binds only
  new chat, search, settings, the sidebar, stop and the composer's keys (`keyboard/keyboard-shortcuts.ts`
  `chatAppBindings`). 搜尋 is a conversation search: the palette offers 新對話, 最近刪除 and 設定, lists each
  conversation once and searches no files (`command-center/root-registration.tsx`, `SHOW_WORKSPACE_COMMANDS`,
  `SHOW_AGENT_CONTROL_COMMANDS`, `SEARCH_FILES`). Settings: 一般 is the language only, sending during a reply always
  queues (CloudCLI refuses a message while a reply runs; `composer/index.tsx` `CHAT_SEND_BEHAVIOR`); 外觀 is 淺色 /
  深色 / 系統, the two text sizes and the content width (`SHOW_DEVELOPER_APPEARANCE`: no code fonts, syntax theme, diff
  preview, CSS font stack, brand or plugin themes).
- **One sidebar row**: the items at the top of the sidebar (新對話 / 最近刪除 / 搜尋), the conversation rows and the
  settings list share one geometry: `density.rowHeight` tall, 8 + 8 px inset, a 14 px label on
  `SIDEBAR_ROW_LINE_HEIGHT` (`styles/density.ts`), the icon in a conversation status slot; the settings items are
  `SidebarHeaderRow variant="compact"`, the same component as the top items, on phones too. Signed-out shell, share
  page, 最近刪除 and the loading skeleton take the same values as the screens they stand for.
- **YYC features** (`src/yyc/`): sent-file cards (`sent-files.tsx`; kept visible when a turn folds,
  `agent-stream/turn-collapse/model.ts`), paths in replies download through CloudCLI, uploads through
  `/api/assets/files` (`composer/actions.ts`), the "preparing the workspace" chip (`sandbox-status.ts`,
  `sandbox-chip.tsx`), typing warms a host (`warm.ts`), the greeting with the person's first name (`greeting.ts`,
  `me.ts`), sharing (`share.tsx`, and the read-only page `app/s/[id].tsx`), the conversation menu
  (`screens/workspace/conversation-menu-items.tsx`).
- **Branding**: YYC's teal as the accent, its logo and icons, Hong Kong Chinese fonts first (`styles/theme.ts`); the
  server writes the instance's product name into the page title.

## The iOS app

The same `paseo/packages/app`, built natively as YYC AI (`dev.de0ch.yycai`, the TestFlight app the SwiftUI shell used
to ship; it no longer builds yyc-ai). What differs from the web build:

- **Server**: the build secret `YYC_SITE` becomes `extra.yycSite` (`app.config.js`, `yyc/site.ts`). The repo is
  public, so the address appears nowhere in it or in CI logs.
- **Sign-in**: Cloudflare Access before anything connects (`yyc/access-gate.tsx`). The router's
  `/api/jarvis/app-signin` hands the Access token back on the app's scheme; it is kept in the Keychain
  (`yyc/access-token.ts`) and sent as `cf-access-token` and the `CF_Authorization` cookie on REST and the socket
  (`runtime/websocket-factory.ts`). An expired token sends the app back to the sign-in screen.
- **Files**: downloaded into the app and handed to the share sheet (`yyc/open-file.ts`).
- **Not there**: push notifications, the camera.
- **Version**: its own, `YYC_APP_VERSION` in `app.config.js` (above the SwiftUI app's 1.0); the build number is the CI
  run.

`.github/workflows/yyc-ios.yml` builds it on a GitHub macOS runner on every push touching `paseo/` (`paseo/ios/`:
prebuild, a simulator launch to the sign-in screen with Maestro, an unsigned archive signed at export with the App
Store Connect key) and uploads to TestFlight. Its secrets: `YYC_SITE`, `ASC_KEY_ID`, `ASC_ISSUER_ID`,
`ASC_PRIVATE_KEY`. CI cannot pass Access, so the chat itself is first exercised on TestFlight.

## Build and run

- `npm --prefix paseo ci` once (its own workspace and lockfile), then `npm run build` builds the app into `dist/`
  (`scripts/build-paseo-ui.mjs`, about 20 minutes on 4 cores) and the server. Metro needs a big heap for the 10 MB
  generated protocol validator: the script gives it 6 GB and `build:web` runs 3 workers. On a machine with 8 GB, stop
  anything else heavy first.
- Development: run the server (`npm run server:dev`, Jarvis local mode needs `JARVIS_LOCAL_WORKSPACE`,
  `JARVIS_CC_OWNER`, a Postgres in `DATABASE_URL`), then `EXPO_PUBLIC_LOCAL_DAEMON=localhost:<port> npm --prefix paseo
  run dev:web`; the app connects to that server instead of Metro's origin. CloudCLI's REST is cross-origin there
  (the CORS answer refuses credentials), so the YYC REST features only work on the built app served by the server.
- A change to `paseo/packages/protocol` needs `npm --prefix paseo run build:protocol` (it regenerates the validators)
  before the app typechecks against it.

## Tests

- Server: `server/modules/paseo/tests/` (tsx --test), in `npm test`.
- App: `.github/workflows/paseo-ui.yml` typechecks the app and runs the tests of the changed areas. Paseo's other
  tests still mention its removed locales and features and are not run.
- Protocol replies: capture a session's frames with a browser (Playwright `page.on('websocket')`) and parse each
  server frame with `WSOutboundMessageSchema` from `paseo/packages/protocol/dist/messages.js`; zero failures is the
  bar for a bridge change.
