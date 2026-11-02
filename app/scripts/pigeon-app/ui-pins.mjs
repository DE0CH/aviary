#!/usr/bin/env node
// The yyc-ai-ui images served within a time window, from the ui pin history in a deploy repository's git log (pigeon
// spec 17.1 step 6, cutover D13, DESIGN 9.6). Every commit of the deploy repository's first-parent history that
// touched the deploy tree is read; the `yyc-ai-ui` image's `newTag` in its kustomizations at that commit is the pin
// from that commit until the next change. An image counts as served within the window when its pin interval overlaps
// [now - hours, now]: the pin in force at the window start counts too.
//
//   node ui-pins.mjs --repo DIR [--ref REF] [--hours 720] [--now ISO] [--tree yyc-ai/deploy] [--json]
//
// Prints one line per image, oldest pin first: `<tag> <first pinned, ISO> <last served, ISO or "now">`; with --json,
// a JSON array of {tag, from, until}. A pin that is not a 12-40 character lowercase hex tag is an error (the tag names
// a claudecodeui commit).
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const IMAGE = 'yyc-ai-ui';

function git(repo, args) {
  return execFileSync('git', ['-C', repo, ...args], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

// The newTag values of every `images:` entry named yyc-ai-ui in a kustomization's text
export function pinsInKustomization(text) {
  const tags = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const start = lines[i].match(/^(\s*)-\s+name:\s*["']?([^"'\s#]+)["']?\s*(#.*)?$/);
    if (!start || start[2] !== IMAGE) continue;
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

function pinsAt(repo, commit, tree) {
  const files = git(repo, ['ls-tree', '-r', '--name-only', commit, '--', tree])
    .split('\n')
    .filter((file) => /(^|\/)kustomization\.ya?ml$/.test(file));
  const tags = new Set();
  for (const file of files) {
    for (const tag of pinsInKustomization(git(repo, ['show', `${commit}:${file}`]))) tags.add(tag);
  }
  return [...tags].sort();
}

export function servedImages({ repo, ref = 'HEAD', hours = 720, now = new Date(), tree = 'yyc-ai/deploy' }) {
  const windowStart = now.getTime() - hours * 3600 * 1000;
  const log = git(repo, ['log', '--first-parent', '--format=%H %cI', ref, '--', tree])
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      const [commit, time] = line.split(' ');
      return { commit, time: new Date(time).getTime() };
    });
  // newest first: each commit's pins are in force from its time until the previous entry's (or now)
  const served = new Map();
  let until = now.getTime();
  for (const { commit, time } of log) {
    for (const tag of pinsAt(repo, commit, tree)) {
      if (!/^[0-9a-f]{12,40}$/.test(tag)) throw new Error(`${commit}: ${IMAGE} tag ${tag} is not a commit hash`);
      const entry = served.get(tag);
      const last = until === now.getTime() ? null : until;
      if (!entry) served.set(tag, { tag, from: time, until: last });
      else entry.from = Math.min(entry.from, time);
    }
    until = time;
    if (time <= windowStart) break;
  }
  return [...served.values()].sort((a, b) => a.from - b.from || a.tag.localeCompare(b.tag));
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const opt = (name, fallback) => {
    const i = args.indexOf(name);
    return i === -1 ? fallback : args[i + 1];
  };
  const repo = opt('--repo');
  if (!repo) {
    console.error('usage: node ui-pins.mjs --repo DIR [--ref REF] [--hours 720] [--now ISO] [--tree yyc-ai/deploy] [--json]');
    process.exit(2);
  }
  const images = servedImages({
    repo,
    ref: opt('--ref', 'HEAD'),
    hours: Number(opt('--hours', '720')),
    now: new Date(opt('--now', new Date().toISOString())),
    tree: opt('--tree', 'yyc-ai/deploy'),
  });
  const iso = (ms) => (ms === null ? 'now' : new Date(ms).toISOString());
  if (args.includes('--json')) {
    console.log(JSON.stringify(images.map((image) => ({ tag: image.tag, from: iso(image.from), until: iso(image.until) }))));
  } else {
    for (const image of images) console.log(`${image.tag} ${iso(image.from)} ${iso(image.until)}`);
  }
}
