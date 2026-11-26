---
name: transcribe
description: Turn the speech in a recording, video or voice message into text — a transcript, 逐字稿, 謄本 — when it has no captions, with the time of each part. Works on any audio or video file (uploads, YYC Drive, Feishu, a link's sound from the youtube skill), in Cantonese, Mandarin, English or a mix.
---

# Transcripts from speech

The speech-to-text model is an API on this computer's network: `POST http://model/v1/audio/transcriptions` with one
piece of sound of at most 30 seconds (longer pieces are cut mid-word). It finds the language itself and answers
`{"text": …}` in Simplified characters. It costs about USD 0.0005 a minute of sound.

1. The model: `M=$(curl -sS http://model/v1/aviary/models | jq -r .transcription.id)`.
2. The sound, mono 16 kHz MP3: `ffmpeg -v error -i IN -vn -ac 1 -ar 16000 -b:a 64k /tmp/video/a.mp3`.
3. Pieces cut in pauses: `speech-times /tmp/video/a.mp3` lists the stretches of sound. Join stretches in order into
   pieces of at most 28 s, each starting at a stretch's start (a stretch longer than 28 s: parts of 25 s). Cut each:
   `ffmpeg -v error -ss START -to END -i /tmp/video/a.mp3 -c copy /tmp/video/p/NNN.mp3`.
4. Each piece, several at a time (a script with 6 at once is right for a long recording):
   `jq -n --arg m "$M" --rawfile a <(base64 -w0 P.mp3) '{model:$m, temperature:0, input_audio:{data:$a, format:"mp3"}}' | curl -sS http://model/v1/audio/transcriptions -H 'Content-Type: application/json' -d @-`.
   Ask again for a piece that failed, or whose text is far longer than its speech could be (over 10 characters a
   second). Music or noise without speech comes back empty: leave it out.
5. Hong Kong Traditional characters: `python -m opencc -c s2hk -i raw.txt -o hk.txt`. Spoken Cantonese stays as spoken.
6. The transcript: the pieces in order, each starting with its time in the recording as `[mm:ss]` (`[h:mm:ss]` past an
   hour). The model does not tell speakers apart: when the person wants speakers, label them from what is said
   (主持人, a guest's name) and say the labels are your reading. Proper names are the model's hearing: check people's
   and companies' names against the video's title, description or the web.
