---
name: youtube
description: Get a video, its sound, its captions, its details or its comments from a link — YouTube (videos, Shorts, live recordings), Now 新聞, ViuTV, TVB. Use for any request to download, watch, transcribe, summarise or quote a video or programme given by its link, or to read a video's comments.
---

# Videos from a link

`yt-dlp` on this computer reads YouTube, Now, ViuTV and TVB directly. Use it; download websites, mirrors (Invidious,
Piped, cobalt) and transcript websites are not needed. Work in `/tmp/video`.

- Details: `yt-dlp -J URL > /tmp/video/v.json` (`title`, `channel`, `upload_date`, `duration`, `view_count`,
  `description`); the caption tracks: `yt-dlp --list-subs URL`.
- Comments, all of them with replies: `yt-dlp --skip-download --write-comments -o '/tmp/video/%(id)s' URL`, then the
  `comments` of `/tmp/video/<id>.info.json`.

## What is said

1. Captions first. The uploader's in the video's language, else YouTube's automatic track of that language: the one
   whose code ends in `-orig` (`yue-orig`, `zh-Hans-orig`, `en-orig`…); never an automatically translated track.
   `yt-dlp --skip-download --write-subs --write-auto-subs --sub-langs 'LANG-orig,LANG' --convert-subs srt -o '/tmp/video/%(id)s' URL`.
   Automatic captions repeat each line as it scrolls: keep each line once.
2. No captions: download the sound only, `yt-dlp -f ba -x --audio-format mp3 -o '/tmp/video/%(id)s.%(ext)s' URL`, and
   follow the `transcribe` skill.

## The footage

To watch, cut or keep a video: `yt-dlp -f 'bv*[height<=1080]+ba/b' --merge-output-format mp4 -o '/tmp/video/%(id)s.%(ext)s' URL`
(a lower height for a long programme when the picture matters less), then the `video` skill. When only the words are
needed, get the sound alone: it is a tenth of the size. A video the person wants to keep goes to `/drive`.

A private, removed, members-only or age-restricted video needs a signed-in account, which this computer does not
have: say so. When yt-dlp says a site changed or formats are missing, `pip install -U 'yt-dlp[default]'` and try once
more.
