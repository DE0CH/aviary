---
name: feishu-files
description: Search, read, create, edit, move and delete files in YYC's shared Feishu folder 「YYC 檔案機械人共享資料夾」 (飛書): Docs, Sheets, Bases (多維表格) and uploaded files such as PDF, Word, Excel and PowerPoint. Use it whenever someone asks about something that may be in the company's Feishu files, asks to find or read a Feishu document, or asks to save, write, update, upload, organise or delete something in Feishu.
---

# YYC's shared Feishu folder

YYC keeps shared files in the Feishu folder 「YYC 檔案機械人共享資料夾」. Staff open it in Feishu themselves, and
through you they search it, read it and change it: every conversation works on the same folder, with the same full
access (read, create, edit, move, delete). There is no per-person permission and no undo button here: a deleted file
goes to Feishu's recycle bin. Delete or overwrite only what the staff
member asked for; when the request is unclear about which file, ask before deleting or replacing.

```sh
F() { curl -sS "$@"; }   # define in each command
B=http://feishu-files/v1
```

Call the API with plain `curl` (no `jq` or other filters), so the chat shows the answer. Give staff the file's link
(`url`) whenever you mention, create or change a file.

## Find and read

```sh
F -G "$B/search" --data-urlencode 'q=季度預算' -d k=10          # mode=hybrid (default) | keyword | semantic
F "$B/files"                                                   # every item: token, kind, name, path, url, status
F "$B/files/<token>"                                           # one item's text, in chunks
F "$B/status"                                                  # "folder": the shared folder's token
```

- **Search** covers the text of everything in the folder and its subfolders, in Chinese (Traditional or Simplified,
  either finds the other) and English. Each hit has `name`, `path`, `url`, `kind`, `heading` (the sheet, table or
  page) and `text` (the passage). `keyword` finds exact words; `semantic` finds passages by meaning; `hybrid` does
  both. Search first, then read the file for the full text.
- **Kinds:** `docx` (Doc), `doc` (old Doc), `sheet`, `bitable` (Base), `file` (an upload), `folder`. Text is read
  from Docs, Sheets, Bases and from uploaded PDF, Word, Excel, PowerPoint and text files; other uploads (images,
  video) are listed with `status` `skipped`.
- The search index follows a change within about a minute. Right after you or someone else changed a file, read it
  live with the Feishu API below instead.

## Docs

```sh
F -X POST "$B/docs" -H 'Content-Type: application/json' \
  -d '{"title": "會議紀錄 2026-11-12", "markdown": "# 決定\n\n- 第一項\n- 第二項"}'      # new Doc; "folder": "<token>" for a subfolder
F -X POST "$B/docs/<document_id>/append" -H 'Content-Type: application/json' -d '{"markdown": "## 補充\n\n……"}'
F -X PUT  "$B/docs/<document_id>" -H 'Content-Type: application/json' -d '{"markdown": "……"}'   # replace the whole body
```

A new Doc answers `document_id`; its link is `https://jcn7i7l4xk0p.feishu.cn/docx/<document_id>`. Markdown headings,
bold, italics, links, lists (nested too), quotes, code blocks and tables become Feishu blocks. `PUT` replaces
everything in the Doc except its title: to change one part, read the Doc, then either append or replace the body with
the full new text. For a long body, write the JSON to a file and send it with `-d @/workspace/body.json`.

## Uploads

```sh
F -X POST -G "$B/upload" --data-urlencode 'name=報告.pdf' --data-binary @/workspace/報告.pdf \
  -H 'Content-Type: application/octet-stream'                    # add --data-urlencode 'folder=<token>' for a subfolder
F "$B/feishu/open-apis/drive/v1/files/<file_token>/download" -o /workspace/報告.pdf     # an upload's bytes
```

At most 20 MB per file. The answer has `file_token`; the link is `https://jcn7i7l4xk0p.feishu.cn/file/<file_token>`.

## Everything else: Feishu's own API

`$B/feishu/open-apis/…` passes a request to Feishu's Open Platform API and returns Feishu's answer as it is
(`{"code": 0, "msg": …, "data": …}`; a `code` other than 0 is an error, and `msg` says why). Use the method, path,
query and JSON body Feishu documents (https://open.feishu.cn/document, the server API of 雲文檔: 雲空間, 文檔, 電子表格,
多維表格); look a call up there when it is not below. It serves Drive, Docs, Sheets and Base
(`open-apis/drive|docx|doc|sheets|bitable/…`). Sharing a file, changing who can open it and transferring ownership are
refused (403): staff do those in Feishu themselves.

```sh
A=$B/feishu/open-apis
J='Content-Type: application/json'

# folders, moving, deleting   (type: file | docx | sheet | bitable | folder | doc)
F "$A/drive/v1/files?folder_token=<folder>&page_size=200"                                    # one folder's items, live
F -X POST "$A/drive/v1/files/create_folder" -H "$J" -d '{"name": "2026 報告", "folder_token": "<folder>"}'
F -X POST "$A/drive/v1/files/<token>/move" -H "$J" -d '{"type": "docx", "folder_token": "<folder>"}'
F -X DELETE "$A/drive/v1/files/<token>?type=docx"                                            # to the recycle bin

# a Doc, live
F "$A/docx/v1/documents/<document_id>/raw_content"                                           # its text now

# Sheets
F -X POST "$A/sheets/v3/spreadsheets" -H "$J" -d '{"title": "預算", "folder_token": "<folder>"}'   # -> spreadsheet_token, url
F "$A/sheets/v3/spreadsheets/<spreadsheet_token>/sheets/query"                               # worksheets: sheet_id, title
F "$A/sheets/v2/spreadsheets/<spreadsheet_token>/values/<sheet_id>!A1:D50"                   # read cells
F -X PUT "$A/sheets/v2/spreadsheets/<spreadsheet_token>/values" -H "$J" \
  -d '{"valueRange": {"range": "<sheet_id>!A1:B2", "values": [["名稱", "金額"], ["甲", 12]]}}'  # write cells
F -X POST "$A/sheets/v2/spreadsheets/<spreadsheet_token>/values_append" -H "$J" \
  -d '{"valueRange": {"range": "<sheet_id>!A1:B1", "values": [["乙", 34]]}}'                    # add rows below the data

# Base (多維表格)
F -X POST "$A/bitable/v1/apps" -H "$J" -d '{"name": "客戶名單", "folder_token": "<folder>"}'        # -> app_token, url
F "$A/bitable/v1/apps/<app_token>/tables"                                                    # tables: table_id, name
F "$A/bitable/v1/apps/<app_token>/tables/<table_id>/fields"                                  # fields: name, type
F -X POST "$A/bitable/v1/apps/<app_token>/tables" -H "$J" \
  -d '{"table": {"name": "客戶", "default_view_name": "表格", "fields": [{"field_name": "名稱", "type": 1}, {"field_name": "金額", "type": 2}]}}'
F -X POST "$A/bitable/v1/apps/<app_token>/tables/<table_id>/records/search?page_size=500" -H "$J" -d '{}'   # read records
F -X POST "$A/bitable/v1/apps/<app_token>/tables/<table_id>/records/batch_create" -H "$J" \
  -d '{"records": [{"fields": {"名稱": "甲", "金額": 5}}]}'
F -X PUT "$A/bitable/v1/apps/<app_token>/tables/<table_id>/records/<record_id>" -H "$J" -d '{"fields": {"金額": 6}}'
F -X DELETE "$A/bitable/v1/apps/<app_token>/tables/<table_id>/records/<record_id>"
```

- A new file needs the folder it goes into: the shared folder's token is `folder` in `$B/status`; a subfolder's
  token is its `token` in `$B/files`. A file created without `folder_token` lands where staff cannot see it: always
  give one.
- Field types of a Base: 1 text, 2 number, 3 single choice, 4 multiple choice, 5 date (milliseconds), 7 checkbox,
  15 link. A new Base comes with one empty default table.
- A Sheet's range is `<sheet_id>!A1:C10`; a new Sheet has one worksheet (ask `sheets/query` for its `sheet_id`).
- Code 99991400 or HTTP 429: too many requests; wait a few seconds and try again. A "no permission" or "not
  found" answer for a file: it is not shared with the whole organisation or with 「YYC 檔案機械人」.
