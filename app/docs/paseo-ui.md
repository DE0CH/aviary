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
| timeline `user_message` `images` / `attachments` (added to the protocol) | the history's user images (data: URLs from the transcript) and files (`<files_input>`, paths under /workspace/uploads) |

Everything a developer machine has (git, terminals, scripts, plugins, worktrees, providers, voice) answers empty or
`rpc_error`, and the server_info features leave it out. Add a feature by answering its messages in
`paseo-connection.ts` and checking the replies against `WSOutboundMessageSchema` (below): the app validates every
frame and drops one that does not parse, usually with "Response validation failed for …".

User attachments: a `user_message` row carries the images (`{url, mimeType}`, data: URLs) and files (`uploaded_file`
attachments, paths relative to /workspace) the user sent, from the history and on the row a send puts on the timeline, so
a reopened chat shows them as the message did when sent. The app keeps such images as `remote-url` attachments
(`attachments/types.ts`: the URL is the storage key, nothing is stored locally).

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
  `me.ts`), sharing (`share.tsx`, and the read-only page `app/s/[id].tsx`: the server sends the snapshot as timeline
  rows built by the same `SessionTimeline`, the page hydrates them like a conversation's history and renders the chat's
  own `AgentStreamView` read only, its files read through the share via `ConversationFilesContext`), the conversation menu
  (`screens/workspace/conversation-menu-items.tsx`).
- **Sent files keep their version** (Deyao, 2026-10-28): each file card carries its SendUserFile call's id
  (`sent` on the tool call's `sentFiles`, paseo-timeline.ts) and downloads with `files/raw?…&sent=<id>`, which reads
  the copy-on-write snapshot the router made before the send (`/jfs/sent/<conversation>/<id>/…`; router.mjs "Sent
  files", DE0CH/jarvis3 docs/CLAUDE-CODE.md "Sent files"), so a later rewrite or delete in /workspace doesn't change
  or break it. Cards from before snapshots, and paths named in replies, read the live file. Shares, 「繼續對話」 and forks
  carry the snapshots; deleting a conversation removes them (`jarvis-clone.ts` clones with `juicefs clone`).
- **View as** (Deyao, 2026-10-28; router.mjs "View as", DE0CH/jarvis3 docs/CHAT.md "View as"): the operator page's
  「View as」 opens the app in a new tab at `/#jarvis-view-as=<token>`. `yyc/view-as.ts` keeps the token in that tab's
  sessionStorage (the admin's other tabs stay themselves) and adds it as `?jarvis_view_as=` to every `apiUrl` (REST,
  file links) and the web socket (`runtime/websocket-factory.web.ts`, also used by the startup connection check in
  `utils/test-daemon-connection.ts`, whose client can become the live one); the router serves the person's own process
  as them, so the list, conversations, files and shares are theirs. The tab keeps the person's replica cache in an
  IndexedDB of its own (`paseo-replica-row-store.view-as.<email>`) and their last conversation under its own key, so it
  never starts from the admin's cached list nor writes into it. The tab is read-only: the message box shows its text
  without an input, the composer's send (button, Enter, queue) and the new-conversation entries (sidebar 新對話
  dimmed, the palette item, the shortcut) do nothing, and typing warms nothing. `yyc/view-as-box.tsx` above the composer is the one box only the admin sees (English, "Visible
  only to you"): whose view it is, and the override switch (off on every load; the box turns red while on) that lets
  the admin send and start conversations as the person. The bridge (`paseo-connection.ts`, viewer from the router's
  `X-Jarvis-Viewer`) records each such send in Ops History before sending (`server/modules/jarvis/jarvis-view-as.ts`;
  no record, no send) and refuses renames, deletes and restores on that socket. The messages look like the person's own.
- **Branding**: YYC's teal as the accent, its logo and icons, Hong Kong Chinese fonts first (`styles/theme.ts`); the
  server writes the instance's product name into the page title.

## Loading

Deyao's order (2026-10-29): a message box people can type in at once, then the connection, then the reply's display
(markdown, code highlighting), then the conversation list, everything else only when used; send waits for the app
("Wait for the app") and for the reply's display ("so nothing goes out before its reply can render"). Decide by
measuring; test on a throttled connection. So:
- **The page frame in `public/index.html`** (`#boot`): the app's own layout (sidebar rows, logo, greeting line, the
  message box; compact below 600px; light and dark from `styles/theme.ts`; the logo inline SVG) in plain HTML, painted
  before any script arrives. React replaces it on its first render; while the app is still connecting,
  `screens/startup-splash-screen.tsx` draws the same frame again (from `window.__yycBootFrame`, saved by the page;
  marked `data-yyc-loading`; inside the app's main column, beside its sidebar, without the frame's own sidebar), so
  the page goes frame -> app without the logo screen. Keep it in step when the layout changes.
- **The message box is always at the bottom** (Deyao, 2026-10-29: "so it doesn't shift"): on every page and screen
  size the composer sits at the bottom of the main column, 16 px from the edges, at most `theme.contentMaxWidth` wide
  (the content-width setting, 820 px by default; the frame reads it as `--b-col-w`), 116 px tall
  (104 px below 720px), as in a conversation. The new-chat screen (`screens/new-workspace-screen.tsx`, the dock never
  centred) and the signed-out screen (`yyc/signed-out-shell.tsx`, whose 「登入」 overhangs a 28 px row) put the
  logo and greeting just above it (the greeting a set 28 px line, `yyc/hero.ts`); the frame draws the same rows
  (signed out, `html.yyc-out`, its second line too; conversation pages, `html.yyc-conv`, the box alone) and the
  sidebar at the saved width, closed, or hidden by focus mode on a workspace route (`panel-state`), also in view-as
  tabs and the desktop build (only the early box is skipped there); share pages (`html.yyc-share`) draw neither
  sidebar nor box. Measured at 1440x900, 390x844, 700 and 650 wide: frame,
  early box and app box within 1 px; the logo too. A conversation that is still loading (`agent-loading` in
  `panels/agent-panel.tsx`) is a loading area, so the early box stays until the composer is there, over a hidden
  `ComposerSlot` (`components/loading-frame.tsx`, `data-yyc-box`) that the box follows on a resize. The conversation
  page's own loading frame (`WorkspaceLoadingFrame`) draws the box at the composer's place, so a send or a drawer tap
  before its chunk arrives never leaves the page without one; if the chunk cannot be fetched it shows a Reload button.
- **The early message box** (`#yyc-early` in `public/index.html`, outside `#root`): a plain textarea and a disabled
  send button over the frame's message box, typable before any script (font, line height and padding from
  `composer/input/input.tsx`, 16 px on touch screens; theme and placeholder language from the saved settings). Its
  inline script exposes `window.__yycEarly`; `yyc/early-input.web.ts` hands the text to the app's message box: the
  first focused composer whose draft has loaded takes the live text and selection in one layout effect (after an IME
  composition ends) and the box goes. A stored draft is never overwritten (the early text goes after it on a new line).
  On a phone with the keyboard up the swap waits for the next tap or typed character (a script may only move the
  keyboard inside one), with the box over the real one, which already holds a copy of the text (no focus), so the app's
  own send button shows: a typed character swaps once it has landed in the box (its `input` event, after an IME
  composition ends; never from keydown, which Android keyboards send before the character); a tap on the box itself
  moves the keyboard over; a first tap anywhere else (the send button, the list, a menu; caught in the capture phase,
  before it acts) or the box losing focus hands the text over without the keyboard; a composer that unmounts after
  that point drops the box, its text kept for its own page only. Enter sends only on wide layouts, through the
  composer's own gate (nothing is sent while its send is off), and nothing is sent before the app is ready: every
  composer's send (new chat and conversation, desktop and phone, a view-as tab with the override on) stays off
  until the reply's display has arrived (`useReplyDisplayReady` in `loading/stages.ts`: the reply chunk and the
  highlighter; a failed fetch is tried again by itself, 1 s, 2 s ... at most 30 s apart), and an Enter or tap before
  that does nothing and leaves the text where it is; the new-chat composer's send also waits until the connection is
  open and the server's projects have arrived (`screens/new-workspace/send-ready.ts`), and a refused send's line
  sits above the box, which stays put. A deep link
  in a browser that never ran the app (no saved host) shows the connecting frame until the page's own server has
  answered or the startup wait gives up, then opens (`navigation/host-runtime-bootstrap.ts`, host routes).
  The box goes on pages without a message box (route guard in `app/_layout.tsx`; the patterns in
  `yyc/early-input-routes.ts`, equal to the page's, and a test classifies every route file: a new route file must be
  placed there), on the welcome, signed-out and share screens, and 1.5 s after the app shows something other than the
  connecting frame or a loading area (`data-yyc-loading`, a lazy main area; the sidebar's placeholder is not one, the
  box never sits there); never during an IME composition or while an adoption waits for one to end. Its text then
  stays in sessionStorage for that page, or `/new` within 60 s; only taking it clears it, and text not yet handed over
  comes back in the box after a refresh of the same page. Skipped when signed out, in view-as tabs and in the desktop
  build.
- **Stages** (`loading/stages.ts`): the entry chunk holds the message box and the connection; on web the host runtime
  bootstrap and the stages start when the entry runs (`index.ts`, with `runtime/host-runtime-start.ts`; never on a
  signed-out page), and the page's language arrives meanwhile as its own chunk (`i18n/translations.web.ts`: the
  saved language, read before the first render as the early box does, `i18n/initial-locale.ts`; zh-HK 31 KB, en 26 KB;
  every key is in both, `resources.test.ts`; the fallback is the language loaded at start, already in the store, so a
  switch that fails shows it rather than raw keys; another language is fetched when chosen and the page keeps the
  current one until it lands; a failed fetch goes through `loadWithRetry`, then is tried again with a backoff capped
  at 30 s, never given up); React renders once the language's text is in the store (`index.ts` loads it again until
  it is; the frame and the early box are there and typable until then; native has both languages in its bundle). Stage 1 starts once the
  server-message validator has arrived or failed: first the reply's display alone (the reply chunk,
  `loading/reply-chunk.ts`: the timeline `agent-stream/view.tsx`, the markdown renderer and the KaTeX and Mermaid
  loaders, with the code highlighter beside it), which turns send on (each through `loadWithRetry`, then tried again
  with a backoff capped at 30 s and at once on the browser's `online` event; after three failed rounds, or an error
  the chunk threw, the composer shows a line and a Reload button beside its disabled send,
  `ReplyDisplayLoadFailed`); stage 2, once stage 1 has settled: the sidebar chunk (`loading/sidebar-chunk.ts`), then
  the conversation page's chunk (`loading/workspace-chunk.ts`: the workspace screen with its header, tabs row, panels
  and git actions; after the list, since a send or a conversation route fetches it at once anyway; before it, the list
  came 0.5 s later on Slow 4G); stage 3 at idle: the hosts chunk (`loading/hosts-chunk.tsx`,
  mounted by `app/_layout.tsx`: the command center and its actions, the dialogs and sheets that open from a store
  (the host chooser, `hosts/host-chooser-modal.tsx`, whose store is `hosts/host-chooser.tsx`, and the host
  confirmation; YYC's conversation rename/share/delete, their store `yyc/conversation-dialog-store.ts`), the download toast,
  the sidebar callouts, the legacy skills migration). The entry keeps the new-chat screen (the landing page) and its
  composer. A lazy area that is merely rendered waits for its stage
  (`agent-stream/lazy-view.tsx`, `components/markdown/lazy-renderer.tsx`, `components/lazy-left-sidebar.tsx`, each with
  its own Suspense, so the composer beside the timeline is there at once), except the conversation page, which fetches
  its chunk as it renders (it holds that page's message box; a loading frame keeps the early box meanwhile); a send
  fetches the reply and conversation chunks at once (`loadChunksForSend`), opening the phone drawer the sidebar. Native
  has one bundle and starts at stage 3.
- **Chunks fetched only when used** (Metro splits every `import()` into its own file; `utils/lazy-with-retry.ts` tries
  a failed fetch three times, each a new fetch (`loading/stages.ts` forgets a failed import), then reloads the page, at
  most once in ten minutes per tab (the mark is never cleared by another chunk loading, so one blocked file cannot
  loop); a chunk that throws while it evaluates is not retried; `ChunkErrorBoundary` keeps a failed fetch local and
  passes any other error on to `RootErrorBoundary`; `components/lazy-area.tsx`
  wraps a component as `lazyArea`, or `lazySheet` for one fetched when first visible): the settings, schedules, sessions,
  open-project and QR-scan screens (`screens/lazy-screens.tsx`), the diff panels (`panels/diff-panel-views.tsx`), the
  file tree, the file-type icons (`components/material-file-icon.tsx`), the catalog providers' icons
  (`components/provider-catalog-icons.ts`), the import-session sheet, the agent profile editor and its settings section,
  the context window details with the usage cards, the image viewer (`components/lazy-attachment-lightbox.tsx`), the
  voice mode's thinking tone (`voice/voice-runtime.ts`), the pull request view, the built-in file views
  (Markdown preview, source, images), the Mermaid + KaTeX diagram runtime (`fence/mermaid/iframe-runtime.web.tsx`), the
  terminal (`panels/terminal-panel.tsx`), KaTeX for inline math (`markdown-runtime/enriched-markdown-katex.ts`, aliased in
  `metro.config.cjs`; the renderer has math off), the relay's end-to-end encryption (`client/src/daemon-client.ts`,
  only for an encrypted relay connection: fetched when such a client is made, awaited before the old transport is
  disposed, and bounded like the validator by the connect timeout, so a stalled fetch fails the attempt and it retries),
  and the full Lucide set (`plugins/icons.ts`, the first time a plugin's Lucide icon is drawn, never when a plugin is
  only checked; a plugin's icon name is checked only for its shape, so a plugin is accepted the same before and after
  the set arrives; a failed fetch is forgotten and the next draw fetches again), the model browser's view
  (`components/lazy-model-browser.tsx`; its hook is `components/model-browser-state.tsx`), the composer's agent
  controls (`composer/agent-controls/lazy.tsx`; YYC's composer never draws them) and context window meter (drawn into
  its fixed 28 px slot), its pull request / issue picker (`composer/forge-picker.tsx`), the dictation and voice
  overlays (drawn over the input, so nothing moves), the plugin attachment picker's Combobox, the plugin evaluator with its UI kit (`plugins/registry.ts` `loadPluginEvaluator`, fetched by
  `plugins/catalog-sync.tsx` only for a catalog with client bundles) and a plugin's own screen (its route). Forge icons
  come from `git/forges/brands.ts`, not the PR-pane view modules. A split pane that has never been shown mounts
  nothing (`components/split-container.tsx`), so the hidden explorer's diff views are not fetched.
- **No eager `__common` chunk:** Expo moves modules two lazy chunks share into `__common`, which the page loads up front.
  `paseo/patches/@expo+metro-config+57.0.13.patch` (applied by `paseo/scripts/postinstall-patches.mjs`) turns that off:
  each lazy chunk keeps its own copy (Metro defines a module id once, so a second copy is skipped). The same patch
  drops from a lazy chunk the modules that every chunk importing it has surely defined by then (its own or its
  importers' ancestors'; an import from the entry leaves only what the entry holds), so a chunk fetched from another
  lazy chunk carries only what is new (the diff panels 80 KB inside the conversation page, the grammars below). The
  cost is bytes in sibling lazy chunks, so a module several lazy areas use is reached through one `import()` of its
  own instead (the code highlighter below). Native has no chunks and is unaffected.
- **Code highlighting loads per language** (`utils/highlight-grammars.ts`): the highlighter (`@getpaseo/highlight`,
  about 15 KB) is one chunk shared by the reply's code blocks and tool calls, the settings preview and the source
  view, fetched the first time any code renders; each language family's grammar is its own chunk
  (`packages/highlight/src/grammars/`, `loadGrammarForFile`), fetched through the grammar loader
  (`packages/highlight/src/grammar-families.ts`, 10 KB, its own chunk with the Lezer runtime every grammar uses, so
  a grammar's chunk is the grammar alone) when code in that language first renders
  (`useGrammarReady` re-renders the block; a failed fetch is retried with backoff while code still waits). Until then
  the code shows plain, in the same font and lines (token styles are colours only; a Read's line-number gutter is
  there from the first frame, also for languages with no grammar), so nothing moves. Swift, Dart, C# and Nix are CodeMirror languages and share one chunk
  with `@codemirror/language`; the editor wraps the others (`@getpaseo/highlight/codemirror`), so the package's main
  entry has no CodeMirror. Every value import of `@getpaseo/highlight` goes through the loader: a static one would
  copy it into that chunk.
- **Smaller entry:** `babel/lucide-per-icon.js` rewrites Lucide imports to one file per icon; startup reads syntax
  colours from `@getpaseo/highlight/themes` (no grammars); zod's `locales` namespace is English only on web
  (`loading/zod-locales.ts`); gesture handler's legacy Hammer.js web implementation, never enabled, resolves to a stub
  on web (`loading/gesture-handler-hammer.js`, `metro.config.cjs`), and so does Reanimated's development-only
  worklets version check (`loading/reanimated-worklets-version.js`, which kept semver in the entry);
  react-native-web's older `@react-native/normalize-colors` resolves to the app's one; no Buffer polyfill on web
  (base64 and UTF-8 through `utils/bytes.ts`; the Buffer-based TextEncoder/TextDecoder fallback is
  `polyfills/text-codec-fallback.native.ts`); the plugin requirements check (semver) comes with the plugin evaluator's
  chunk; the desktop browser automation handler and the released-host usage icons are fetched when used; the new-chat
  screen has no setup form (it was never drawn).
- **The server-message validator** (`@getpaseo/protocol` generated AOT, 4 MB) is its own chunk, fetched in parallel
  from the moment the client module loads; a connect waits for it only while it is still on its way, at most the
  connect timeout (a failed fetch is forgotten, so the next attempt fetches again)
  (`client/src/daemon-client.ts` `loadWSOutboundValidator`; client tests load it in `src/test-setup.ts`).
- **No render-blocking CSS:** the terminal's stylesheet is the only one Expo hoists into the page;
  `scripts/build-paseo-ui.mjs` marks it `media="print"` until loaded.
- **Budgets:** `scripts/check-chunks.mjs` (run by the build) prints every chunk's raw, gzip -9 and brotli q4 size and
  checks the entry (at most 1,100 KB brotli) and the reply chunk (at most 450 KB); report only, `--strict` fails.
- **Where it stands** (brotli q4, 2026-10-29): the page loads one script, `index` (1,093 KB, under the budget; 1,119 KB
  with the attach menu's surface, the forge icons and expo-document-picker/expo-file-system; 1,141 KB with Buffer,
  semver, the desktop browser automation handler and the usage icons; 1,241 KB with both
  languages, the composer's pickers and overlays, the host sheets and the gesture handler's Hammer.js; 1,512 KB before
  the conversation page, the plugin evaluator, the agent controls and the rest left it, 2,273 KB with `__common`,
  3,102 KB before the stages); with it the page's language (zh-HK 31 KB or en 26 KB) and the validator 278 KB; then
  the reply chunk 360 KB with the highlighter 15 KB (send turns on), the sidebar 178 KB, the conversation chunk
  248 KB, the hosts 102 KB; a grammar per language when code first shows (the loader 10 KB once, then 1-50 KB each;
  Swift/Dart/C#/Nix 154 KB). Lazy chunks carry their own copies of modules the entry no longer holds (the sidebar grew
  from 88 KB; the menu engine, 16 KB, is in the reply, sidebar, conversation and attach-menu chunks). The attach menu's surface and
  items are their own chunk (`composer/input/attachment-menu-content.tsx`, 19 KB), fetched at stage 3 or on the
  first hover or press of the attach button, whichever comes first; the button and the menu's state stay in the
  composer. The web picks files with a file input, so the native document picker is behind
  `hooks/document-picker-files.ts` (`.web.ts` is a stub). What remains is the app shell (expo-router with React
  Navigation, react-native-web, react-dom, Reanimated with its layout animations, gesture handler, bottom sheet,
  keyboard controller, unistyles: about half), the protocol and zod, the host runtime, its stores and the new-chat
  screen with its composer (its autocomplete and dictation hooks). Expo's tree shaking
  (`EXPO_UNSTABLE_TREE_SHAKING` with `EXPO_UNSTABLE_METRO_OPTIMIZE_GRAPH`) is untried: its export ran out of memory
  in an 8 GB session.
  Timings, cold, Chrome's Slow 4G, desktop, the page served locally through `brotli-proxy.mjs` with no server behind it
  (so no connection; 140ed041 -> 9e7c6bf6 -> now): the early box takes keys at 0.6-1.2 s in all three, the entry 8.1 ->
  7.5 -> 7.45 s, the validator 10.7 -> 10.3 -> 10.2 s, the reply chunk requested as the validator lands and the reply
  display (send on, once connected) 14.5 -> 12.9 -> 12.9 s, the conversation chunk 14.8 s, the sidebar 16.3 s, the
  hosts 17.5 s (phone the same within 0.1 s). Earlier, signed in through a
  test router (140ed041): typable 0.65 s, composer 11.6 s, send 12.6 s, reply chunk 15.0 s, list 16.5 s.
- Measure with claude-env `.claude/skills/yyc-ai/load-timing.mjs` (cold cache, Chrome's Fast/Slow 4G) through
  `brotli-proxy.mjs`.

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
