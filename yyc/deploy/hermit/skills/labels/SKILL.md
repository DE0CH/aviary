---
name: labels
description: Make and print YYC's labels (name tags, table cards, signs) on its NIIMBOT M3 label printer, and read or change YYC's shared label templates. Use it whenever someone asks for labels, name tags, table cards, a guest list for labels, printing labels, or a change to a label template.
---

# Labels

YYC prints labels on a NIIMBOT M3. Staff ask you for labels or for changes to a template. You draw the labels and
send them to print; sending gives a link to a print page, where the staff member connects the printer and prints them,
or cancels and tells you in this chat what to change.

## The API

```sh
L() { curl -sS "$@"; }   # define in each command
B=http://labels/api/v2
```

`$B/openapi.json` describes every endpoint. A refused request answers JSON with `error` and a `message` saying what to
do.

## The label format

Every label you send is a bitmap in exactly this format; nothing else is accepted (no PNG, SVG, PDF or JPEG):

- **848 × 590 dots**: the printable 72 × 50 mm at 300 dots per inch, landscape, one label per bitmap.
- **1 bit per dot:** 1 = black, 0 = white. No grey: the printer's ribbon spreads ink, so draw in grey and cut at a
  threshold (128), never dither text. Dither only a photo.
- **Rows top to bottom, 106 bytes per row**, the leftmost dot in the highest bit of each byte. One label is
  590 × 106 = 62,540 bytes, with no header.

With Pillow:

```python
from PIL import Image, ImageDraw
img = Image.new("L", (848, 590), 255)            # draw on this: 11.81 dots per mm
...
bits = img.point(lambda v: 255 if v < 128 else 0).convert("1").tobytes()   # 62,540 bytes
img.save("/tmp/label.png")                        # to look at
```

Before sending, look at what you drew (save PNGs and open them): the text fits, nothing is cut off at the edges, and
for a list, check the guests whose names and institutions are longest.

## Templates

YYC keeps shared templates: the layouts it uses (the standard name tag, table cards, a forum's name tags …). Each
holds `design.json` (the layout), `design.md` (how to draw labels from it: instructions and code), four example
labels, and its own files (the drawing code, pictures, a guest list). Start from the template that fits and follow
its `design.md`, so labels look the same whoever asks for them.

```sh
L "$B/templates"                 # every template: id, name, version, lock, files
L "$B/templates/forum"           # one
T=/tmp/templates/forum; mkdir -p $T && L "$B/templates/forum" | jq -r '.files[].path' | while read -r f; do
  mkdir -p "$T/$(dirname "$f")"; L "$B/templates/forum/files/$f" -o "$T/$f"; done   # all its files
L "$B/fonts"                     # fonts: family, the name staff see, weights, files
L --compressed "$B/fonts/<file>" -o /tmp/fonts/<file>                                # one font file
```

Staff see every template with its four examples on the templates page, https://labels.yyc-internal.com/.

## Sending labels to print

```sh
cat /tmp/out/0*.bin > /tmp/print.bin           # the labels one after another, in print order
L -X POST "$B/prints" --url-query "title=論壇嘉賓名牌" --url-query "template=forum" \
  -H 'Content-Type: application/octet-stream' --data-binary @/tmp/print.bin
```

`title` is shown on the print page; `template` names the template the labels come from; `density` (1–5, default 1)
is the print darkness, if the template's `design.json` gives one. At most 500 labels in one request. The answer has the
request's `id`, the number of labels and the `page` link: give the staff member that link and say how many labels it
prints.

`L "$B/prints/<id>"` shows what happened: `status` is `waiting`, `printing`, `printed` (and `printed`: how many) or
`cancelled`. A cancelled request means the staff member wants something changed: ask what, then send a new request.
A page prints its labels once; for more copies, send them again.

## Changing a template: the lock

Changing a template needs its lock. One person holds a template's lock at a time: the staff member who asked for it,
in all their chats, until it is released.

1. Take the lock: `L -X POST "$B/templates/<id>/lock"`.
2. Save the whole template (its files replace the old ones):

   ```sh
   L -X PUT "$B/templates/<id>" -F "name=論壇名牌" -F "design.json=@$T/design.json" -F "design.md=@$T/design.md" \
     -F "examples/1.bin=@$T/examples/1.bin" -F "examples/2.bin=@$T/examples/2.bin" \
     -F "examples/3.bin=@$T/examples/3.bin" -F "examples/4.bin=@$T/examples/4.bin" -F "files/render.py=@$T/files/render.py"
   ```

   `name` is what staff see. The four examples are labels drawn from the template that show its range (a short and a
   long Chinese name, a short and a long English name and institution, or what fits the template); draw them again
   whenever the template changes. Keep `design.md` right for whoever uses the template next.
3. Release it once the change is done: `L -X DELETE "$B/templates/<id>/lock"`.

A new template is the same: pick a short id (lowercase letters, digits and `-`), take its lock, save, release.

If someone else holds the lock, step 1 answers `409` with who it is and since when:

```json
{"error": "locked", "holder": "jackie.chan@yyc.hk", "since": "2026-11-04T09:12:00Z"}
```

Tell the staff member who holds it and since when, and give them two choices: ask that person to release it, or take
it over. Explain it in everyday words, without "lock" or "409": for example, "Jackie Chan has been changing the name
tag template since 9:12, and only one person can change it at a time. You can ask Jackie to finish, or take it over
from Jackie if they have forgotten or are stuck. Which would you like?"

Take it over only when the staff member says so after hearing who holds it:

```sh
L -X POST "$B/templates/<id>/lock/override"
```

The lock is then theirs, and the server records who took it from whom.

If a change is refused because the lock was taken over (`409` with `"error": "lock_taken_over"`, `takenBy` and `at`),
the change was not saved. Tell the staff member who took the template over and when, in everyday words, and that they
can ask that person to hand it back or take it back the same way.

Don't change a template without the lock, and don't make a copy of a template to get around it. Labels can still be
drawn from a template as it is while someone else holds its lock.

## The print page

What the staff member sees at the link, so you can walk them through it:

- **Where:** open the link in Chrome or Edge on a computer near the printer (or an Android phone). Safari and Firefox
  show the labels but can't print.
- **1 連接打印機:** opens the browser's Bluetooth window; pick the M3 there (the printer must be on).
- **2 打印:** prints every label in order, each on its own; progress shows on the page, with 停止. If a label fails,
  the page says why in plain words (no ribbon, label not feeding, lid open …) and offers to continue from that label.
- **取消，不打印:** the page then asks them to come back to this chat and say what to change.
- Below the steps, every label as it prints; printed ones are marked 已打印.
