# Editing video on this computer (`clip`, `speech-times`, `edit`)


Editing here is exact: the tools cut, place, move and draw text and graphics over the footage that exists. Nothing
invents new footage or speech. If the user asks for something that needs new footage (a different background, an object that was never
filmed), say that these tools cannot do it.

## Where to work

Video files are large. Work in a folder outside the conversation's workspace, on the computer's temporary disk
(`mkdir -p /tmp/video && cd /tmp/video`): source footage, `work/` (clips, stills, plans) and the engine's cache all
stay there, and only the finished video is copied into `/workspace` to be sent. What is in `/tmp` is gone when the
computer is released, so keep the plan files you may need again (`work/*.json` are small) in `/workspace` too.
Length of a video: `ffprobe -v error -show_entries format=duration -of csv=p=0 FILE`.

## Seeing and hearing video: `clip` + read_file

You see and hear footage yourself. A raw phone video is far too big to open, so first cut the stretch you want to a
small copy:

```bash
clip footage/NAME.MP4                       # the whole file (at most 300 s per clip)
clip footage/NAME.MP4 --start 20 --end 50   # a stretch;  --fps 4 for fast action (default 2 frames per second)
```

It prints two files, the picture (`.mp4`, no sound) and the sound (`.mp3`). Open BOTH with read_file in the same
step; they then stay in the conversation and you can look and listen again while you plan. A sound track inside a
video file is not heard, which is why the sound is its own file. Second 0 of a clip is its `--start` in the original:
add `--start` to every time you take from it.

Never read_file a source video or a finished one directly: cut it with `clip` first.

For exact cut points use `speech-times FILE` (same `--start`/`--end`): it measures from the sound track where each
stretch of sound starts and ends, split at pauses of 0.25 s or more:

```
  4.    5.18 -    7.38   (pause before: 0.40 s)
  5.    7.86 -    9.52   (pause before: 0.48 s)
```

These times are measured, not guessed: cut on them and use them as caption times. What is said in each stretch you
hear yourself in the clip; match your transcript to the stretches in order. Your own sense of time in a clip is only
good to about a second, so never cut on it alone.

Always watch and hear footage before cutting it: file names do not tell you what is said or where a sentence ends.

To look at a single frame closely: `ffmpeg -v error -ss 3.2 -i FILE -frames:v 1 -vf scale=-2:640 work/f.jpg`, then
read_file it. Never open full-size frames.

## Editing: `edit` and a plan file

Write a plan (JSON) in `work/`, then:

```bash
edit example                      # a full example plan and every core field explained: read it once before the first plan
edit effects                      # the motion graphics: camera moves, text styles, graphics, transitions, sound
edit assets                       # the library: music, sound effects, animated emoji, fonts
edit check work/plan.json         # validate; prints where each clip lands in the output (seconds)
edit frame work/plan.json --at 3.5 --out work/f.png    # one still of the finished video at output second 3.5; open it with read_file
edit render work/plan.json        # render to the plan's "output"
```

A small plan:

```json
{
  "output": "out/joke.mp4",
  "captionStyle": {"look": "outline", "animate": "pop"},
  "clips": [
    {"card": "機械人講笑話", "seconds": 1.5, "transition": "fade"},
    {"src": "footage/joke.mp4", "start": 0, "end": 4.5, "transition": "slide-left",
     "captions": [{"start": 0.0, "end": 4.5, "text": "哈哈，你叫我講笑話，\n我諗咗好耐喎"}]},
    {"src": "footage/joke.mp4", "start": 11, "end": 14.5,
     "captions": [{"start": 11.0, "end": 14.5, "text": "因為我哋冇得閒呀"}]}
  ],
  "texts": [{"text": "機械人笑話", "start": 1.6, "end": 4, "position": "top", "style": "title"}]
}
```

What matters:

- `clips` is the timeline in order: stretches of video (`src`, `start`, `end` in the source file's seconds), still
  pictures (`image`, `seconds`) and text cards (`card`, `seconds`). Default output is 1080x1920 at 30 fps; `fit` says
  how footage of another shape fills it (`cover` crops, `contain` adds bars, `blur` puts it over a blurred copy).
- `captions` belong to a clip and use the SOURCE file's seconds, exactly as `speech-times` reports them. They move
  with the clip, so reordering or speeding up a clip never needs caption times recalculated. Keep a caption to one
  or two short lines (`\n` breaks the line; about 12 Chinese characters per line at the default size). `*stars*`
  around a word colour it as the emphasised word.
- `texts` and `graphics` at the top level use OUTPUT seconds; inside a clip they count from that clip's beginning,
  or, with `"time": "source"`, use the source file's seconds like captions (the easy way to hit a spoken word).
- A `transition` on an item is how it changes into the next one; the two overlap, so each transition shortens the
  video by its length. No transition is a hard cut, which is right for most cuts inside one scene.
- `speed`, `volume` per clip; `music` plays under everything, quieter under speech, and fades out at the end.
- The sound is loudness-normalised at the end.

## Motion graphics

`edit effects` prints all of it with an example; read it once before a video that should look designed (a promo, a
social clip, anything "flashy", "fun", "with graphics"). What the plan can ask for:

- `theme`: one word sets the whole look: `clean` (default, plain), `bold` (big yellow emphasis, the social-video
  look), `neon`, `cute`, `news`, `comic`. Pick one that fits the request before anything else.
- Camera on a clip: `motion` (`zoom-in`, `zoom-out`, `pan-…`, `shake`, `pulse`, `handheld`), `punches` (a sudden
  punch-in at a spoken word, in source seconds), `look` (colour grade), `inset` (the clip as a rounded card over an
  animated background).
- Texts with a `style` (`impact`, `stamp`, `bubble`, `burst`, `tag`, `neon`, `gradient`, `highlight`, `lower-third`,
  `counter`, …), an entrance `animate` (`pop`, `drop`, `stamp`, `chars`, `typewriter`, `slide-…`) and a `loop`.
- Graphics: animated `emoji`, `emoji-rain`, `confetti`, `arrow`, `circle` (hand-drawn ring), `spotlight`,
  `speedlines`, `sparkles`, `flash`, `light-leak`, `bars`, a logo `image`.
- Transitions: `flash`, `zoom`, `whip-left/right/up/down`, `glitch`, `spin`, `flip`, `clock-wipe`, `iris`, besides
  fade, slide and wipe.
- Sound: `"sfx": "auto"` puts a fitting whoosh, pop or hit on every transition, popping text and graphic; `sounds`
  places single effects. `music` from the library.
- Cards animate by themselves; give them an `emoji` and a `*starred*` word.

The library is in `assets/`: `edit assets music` (49 tracks by mood with beats per minute), `edit assets sfx` (119
effects and the shortcut names), `edit assets emoji` (94 animated emoji; any other emoji is drawn still),
`edit assets fonts`. All free to use in videos, commercial ones included. `edit beats FILE` lists a track's beat times.

How to make it feel edited rather than decorated:

- The first two seconds decide whether people keep watching: open on the strongest line or picture, not on a slow title.
- Something should change every two to four seconds: a cut, a punch-in, a text popping in, a graphic. A punch-in
  on the word that matters does more than any ornament.
- Tie every graphic to what is said or shown at that moment (the 😂 on the punchline, the arrow at the thing being
  talked about, a `counter` for a number, a `stamp` on the claim). Use `"time": "source"` with the `speech-times` of
  that sentence so it lands on the word.
- Mark one or two words per caption with `*stars*`, not whole lines.
- One theme, two or three text styles, and a few kinds of graphic per video; the same kind for the same job. Every
  effect at once looks cheap.
- Keep faces and the product clear: put stickers and labels beside them, never on them. Captions sit low; keep
  other texts in the top third or at the sides.
- Music: choose a mood that fits, keep it under the speech (the default does), and where there is no speech (a
  dance, a montage) cut on the beat (`edit beats`).
- When the user asks for "more", add motion and sound on the moments that matter before adding more decoration.

Check motion with `edit sheet`, which lays several stills side by side with their times:

```bash
edit sheet work/plan.json --from 3 --to 6 --every 0.2 --out work/s.jpg     # how a text pops in, a punch-in, a transition
edit sheet work/plan.json --out work/all.jpg                               # the whole video in about 16 stills
```

Cut speech at sentence ends: take `start` about 0.1 s before a `speech-times` stretch's start and `end` about 0.15 s after
its end (staying inside the pause), never mid-word. Neighbouring sentences that belong together can stay one clip.

## The loop

1. `clip` the candidate footage and read_file both files: what happens, what is said. `speech-times` for the exact times.
2. Write `work/NAME.json`; `edit check` it.
3. `edit frame` at one or two moments with captions or titles, or `edit sheet` over a stretch with motion, and look
   at the stills (text size, position, nothing covering a face).
4. `edit render`.
5. `clip` the result and watch and hear it once: do the cuts land on sentence ends, do captions match
   the speech? Fix the plan and render again if not. Clips already prepared are reused, so a second render is faster.
6. Give the user the file.

A plan problem is reported with the item it is in (`clips[2]: end 20 s is past the end of …`): fix that item.

## Beyond the plan: ffmpeg, and the engine's source

For something a plan cannot express, prepare the piece with ffmpeg first and use the result as a clip (`work/x.mp4`):

```bash
ffmpeg -i footage/IN.MP4 -vf "crop=iw:ih/2:0:ih/4" -c:a copy work/cropped.mp4          # a custom crop
ffmpeg -i a.mp4 -i voice.wav -map 0:v -map 1:a -c:v copy -c:a aac -shortest work/dub.mp4   # replace the sound
```

For a look none of `edit effects` gives, write your own HTML block and name it in the plan's `html` list (`edit
effects`, "YOUR OWN HTML"): an HTML fragment laid over the frame for a stretch of the video, animated with a paused
GSAP timeline or CSS. The engine renders with HyperFrames (HTML rendered to video, frame by frame, in a headless
browser); its source is in `/opt/engine` for reference and is not yours to change.
