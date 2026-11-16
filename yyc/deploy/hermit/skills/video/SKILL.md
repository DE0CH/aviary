---
name: video
description: Edit videos — watch and hear footage, cut, join, caption, add titles, motion graphics, sound effects and music, export. Use for any request to view, find, cut, combine, caption or produce a video.
---

# Video editing

Tools on this computer: `clip`, `speech-times`, `edit`. **Read `/opt/engine/GUIDE.md` in full first**: it is their
manual. Seeing and hearing footage needs a model that takes video and sound; if a clip shows you nothing, say so
(the person can pick such a model in the model menu).

Footage: attached files are in `/workspace/uploads/`; the company's is in Feishu (`feishu-files` skill lists items;
download one with `curl -sS "http://feishu-files/v1/feishu/open-apis/drive/v1/files/<token>/download" -o FILE`).
Work in `/tmp/video` (footage is large). Copy the finished video into `/workspace` and send it (at most 64 MB).
Chinese captions are Traditional (Hong Kong); Cantonese is captioned as spoken.
