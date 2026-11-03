#!/usr/bin/env node
// The images of one name served within a time window, from the pin history in a deploy repository's git log (pigeon
// spec 17.1 step 6, cutover D13, DESIGN 9.6): `yyc-ai-ui` (the seeds of pigeon-app) and `pigeon-app` itself (its
// builds stay while served). Every commit of the deploy repository's first-parent history that touched the deploy
// tree is read; the image's `newTag` in its kustomizations at that commit is the pin from that commit until the next
// change. An image counts as served within the window when its pin interval overlaps [now - hours - margin, now]: the
// pin in force at the window start counts too.
//
// Times are committer dates, which are not deploy times: a commit is pushed, and its rollout finishes, some time after
// it was made, and dates need not grow along first-parent order (a clock off, a commit made long before its push). So
// a pin's end is the later of its own commit's date and the replacing commit's (the replacement was pushed after
// both), the window is widened by --margin-hours (default 24) for the push and rollout delay, and the whole
// first-parent log is read (its dates, no blobs) with no early stop on a date; only commits whose pin can overlap the
// window have their kustomizations read.
//
//   node pins.mjs --repo DIR [--image yyc-ai-ui] [--ref REF] [--hours 720] [--margin-hours 24] [--now ISO]
//                 [--tree yyc-ai/deploy] [--json] [--require-while-pinned]
//
// Prints one line per image, oldest pin first: `<tag> <first pinned, ISO> <last served, ISO or "now">`; with --json,
// a JSON array of {tag, from, until}. A pin that is not a 12-40 character lowercase hex tag is an error (the tag names
// a claudecodeui commit). --require-while-pinned exits 1 when nothing is listed although the ref still pins the image
// (an empty list is right once the pin has been gone for longer than the window, e.g. yyc-ai-ui after the switch's
// cleanup; it is wrong while the image is pinned).
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

function git(repo, args) {
  return execFileSync('git', ['-C', repo, ...args], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

// The newTag values of every `images:` entry with this name in a kustomization's text
export function pinsInKustomization(text, image = 'yyc-ai-ui') {
  const tags = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const start = lines[i].match(/^(\s*)-\s+name:\s*["']?([^"'\s#]+)["']?\s*(#.*)?$/);
    if (!start || start[2] !== image) continue;
    const indent = start[1].length;
    for (let j = i + 1; j < lines.length; j++) {
      const line = lines[j];
      if (line.trim() === '' || line.trim().startsWith('#')) continue;
      const lead = line.match(/^(\s*)/)[1].length;
      // the next list item or a key outside the entry ends it
      if (lead <= indent) break;
      const tag = line.match(/^\s*newTag:\s*["']?([^"'\s#]+)["']?/);
      if (tag) {
        tags.push(tag[1]);
        break;
      }
    }
  }
  return tags;
}

export function pinsAt(repo, commit, tree, image) {
  const files = git(repo, ['ls-tree', '-r', '--name-only', commit, '--', tree])
    .split('\n')
    .filter((file) => /(^|\/)kustomization\.ya?ml$/.test(file));
  const tags = new Set();
  for (const file of files) {
    for (const tag of pinsInKustomization(git(repo, ['show', `${commit}:${file}`]), image)) tags.add(tag);
  }
  return [...tags].sort();
}

export function servedImages({
  repo,
  image = 'yyc-ai-ui',
  ref = 'HEAD',
  hours = 720,
  marginHours = 24,
  now = new Date(),
  tree = 'yyc-ai/deploy',
}) {
  const windowStart = now.getTime() - (hours + marginHours) * 3600 * 1000;
  const log = git(repo, ['log', '--first-parent', '--format=%H %cI', ref, '--', tree])
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      const [commit, time] = line.split(' ');
      return { commit, time: new Date(time).getTime() };
    });
  // newest first: each commit's pins are in force from its time until the newer entry replaced them (or now)
  const served = new Map();
  for (let i = 0; i < log.length; i++) {
    const { commit, time } = log[i];
    const until = i === 0 ? null : Math.max(time, log[i - 1].time);
    if (until !== null && until < windowStart) continue;
    for (const tag of pinsAt(repo, commit, tree, image)) {
      if (!/^[0-9a-f]{12,40}$/.test(tag)) throw new Error(`${commit}: ${image} tag ${tag} is not a commit hash`);
      const entry = served.get(tag);
      if (!entry) served.set(tag, { tag, from: time, until });
      else {
        entry.from = Math.min(entry.from, time);
        if (entry.until !== null) entry.until = until === null ? null : Math.max(entry.until, until);
      }
    }
  }
  return [...served.values()].sort((a, b) => a.from - b.from || a.tag.localeCompare(b.tag));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const opt = (name, fallback) => {
    const i = args.indexOf(name);
    return i === -1 ? fallback : args[i + 1];
  };
  const repo = opt('--repo');
  if (!repo) {
    console.error(
      'usage: node pins.mjs --repo DIR [--image yyc-ai-ui] [--ref REF] [--hours 720] [--margin-hours 24] [--now ISO] [--tree yyc-ai/deploy] [--json] [--require-while-pinned]',
    );
    process.exit(2);
  }
  const image = opt('--image', 'yyc-ai-ui');
  const ref = opt('--ref', 'HEAD');
  const tree = opt('--tree', 'yyc-ai/deploy');
  const images = servedImages({
    repo,
    image,
    ref,
    hours: Number(opt('--hours', '720')),
    marginHours: Number(opt('--margin-hours', '24')),
    now: new Date(opt('--now', new Date().toISOString())),
    tree,
  });
  if (args.includes('--require-while-pinned') && images.length === 0) {
    const current = pinsAt(repo, ref, tree, image);
    if (current.length) {
      console.error(`::error::${ref} pins ${image} (${current.join(', ')}) but no pin of it was found in the window`);
      process.exit(1);
    }
    console.error(`${image}: not pinned at ${ref} and no pin within the window: nothing to list`);
  }
  const iso = (ms) => (ms === null ? 'now' : new Date(ms).toISOString());
  if (args.includes('--json')) {
    console.log(JSON.stringify(images.map((image) => ({ tag: image.tag, from: iso(image.from), until: iso(image.until) }))));
  } else {
    for (const image of images) console.log(`${image.tag} ${iso(image.from)} ${iso(image.until)}`);
  }
}
