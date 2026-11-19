#!/usr/bin/env node
// The builds of one image served within a time window, from the deploy run's records (docs/DEPLOY.md): every deploy of
// main records a GitHub Deployment of DE0CH/aviary per environment (`yyc-ai`, `jarvis`) whose payload names each image's
// tag ({"tags": {"pigeon-app": "<sha12>", …}}). Since the monorepo the tags are not in git, so this replaces pins.mjs
// for builds deployed from aviary; pins.mjs still reads DE0CH/yyc-code's pin history for the builds before it.
//
// A tag is served from its deployment's creation until the next deployment of that environment names another tag (or
// now). It counts when that interval overlaps [now - hours - margin, now]; the margin covers the rollout's delay.
//
//   node deployments.mjs --repo OWNER/NAME --environment yyc-ai [--image pigeon-app] [--hours 720] [--margin-hours 24]
//                        [--now ISO] [--json]
//
// Prints one line per tag, oldest first: `<tag> <first deployed, ISO> <last served, ISO or "now">` (pins.mjs's
// format); with --json, a JSON array of {tag, from, until}. Reads GitHub with GH_TOKEN.
import { pathToFileURL } from 'node:url';

/** deployments: [{created_at, tags}] → [{tag, from, until}] overlapping the window, oldest first */
export function served(deployments, { image, hours, marginHours, now }) {
  const sorted = [...deployments].sort((a, b) => a.created_at.localeCompare(b.created_at));
  const start = new Date(now.getTime() - (hours + marginHours) * 3600e3).toISOString();
  const intervals = [];
  for (let i = 0; i < sorted.length; i++) {
    const tag = sorted[i].tags?.[image];
    if (!tag) continue;
    let until = 'now';
    for (let j = i + 1; j < sorted.length; j++) {
      const next = sorted[j].tags?.[image];
      if (next && next !== tag) {
        until = sorted[j].created_at;
        break;
      }
    }
    if (until !== 'now' && until < start) continue;
    intervals.push({ tag, from: sorted[i].created_at, until });
  }
  const byTag = new Map();
  for (const x of intervals) {
    const had = byTag.get(x.tag);
    if (!had) byTag.set(x.tag, { ...x });
    else {
      if (x.from < had.from) had.from = x.from;
      if (had.until !== 'now' && (x.until === 'now' || x.until > had.until)) had.until = x.until;
    }
  }
  return [...byTag.values()].sort((a, b) => a.from.localeCompare(b.from));
}

async function fetchDeployments(repo, environment, token) {
  const out = [];
  for (let page = 1; page <= 20; page++) {
    const r = await fetch(`https://api.github.com/repos/${repo}/deployments?environment=${environment}&per_page=100&page=${page}`, {
      headers: { Accept: 'application/vnd.github+json', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    });
    if (!r.ok) throw new Error(`deployments of ${repo} ${environment}: HTTP ${r.status}`);
    const list = await r.json();
    for (const d of list) out.push({ created_at: d.created_at, tags: d.payload?.tags ?? {} });
    if (list.length < 100) break;
  }
  return out;
}

function arg(args, name, fallback) {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const repo = arg(args, 'repo');
  const environment = arg(args, 'environment');
  if (!repo || !environment) {
    console.error('usage: deployments.mjs --repo OWNER/NAME --environment ENV [--image pigeon-app] [--hours 720] [--margin-hours 24] [--now ISO] [--json]');
    process.exit(2);
  }
  const list = served(await fetchDeployments(repo, environment, process.env.GH_TOKEN), {
    image: arg(args, 'image', 'pigeon-app'),
    hours: Number(arg(args, 'hours', '720')),
    marginHours: Number(arg(args, 'margin-hours', '24')),
    now: new Date(arg(args, 'now', new Date().toISOString())),
  });
  if (args.includes('--json')) console.log(JSON.stringify(list));
  else for (const x of list) console.log(`${x.tag} ${x.from} ${x.until}`);
}
