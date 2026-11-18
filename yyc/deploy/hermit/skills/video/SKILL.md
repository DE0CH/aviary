---
name: video
description: Edit videos — watch and hear footage, cut, join, caption, add titles, motion graphics, sound effects and music, export. Use for any request to view, find, cut, combine, caption or produce a video.
---

# Video editing

Tools on this computer: `clip`, `speech-times`, `edit`. **Read `/opt/engine/GUIDE.md` in full first**: it is their
manual.

Seeing and hearing footage needs a model that takes video and sound; if a clip shows you nothing, say so (the
person can pick such a model in the model menu).

**Footage lives in YYC Drive**, the company's shared folder, at `/drive` on this computer: the same files staff see
at https://drive.yyc-internal.com. Look there first; attached files are in `/workspace/uploads/`.

Footage in Feishu (`feishu-files` skill lists items): suggest keeping it in YYC Drive instead and offer to copy it
there yourself (`curl -sS "http://feishu-files/v1/feishu/open-apis/drive/v1/files/<token>/download" -o
"/drive/<folder>/<name>"`). The Feishu original stays.

Work in `/tmp/video` (footage is large; never write work files into `/drive`). Save the finished video in `/drive`
beside its footage unless the person names a folder, and tell them where it is. Also copy it into `/workspace` and
send it when it is at most 64 MB. Chinese captions are Traditional (Hong Kong); Cantonese is captioned as spoken.
