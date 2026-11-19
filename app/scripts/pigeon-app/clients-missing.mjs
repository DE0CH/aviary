#!/usr/bin/env node
// The dispatched commits that aviary's testing/paseo-contract/clients.txt does not list yet
// (.github/workflows/pigeon-app.yml waits until there are none).
//
//   node clients-missing.mjs <clients.txt> <file with one 40-hex commit per line>
//
// Prints the missing commits, space-separated (nothing when all are listed). clients.txt lines are
// `<commit> <kinds> <commit time>`; any kind counts (a pre-Paseo ui commit is listed as web-legacy).
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function missingClients(clientsText, commits) {
  const listed = new Set();
  for (const line of clientsText.split('\n')) {
    const match = line.match(/^([0-9a-f]{40})\s+\S+/);
    if (match) listed.add(match[1]);
  }
  return commits.filter((commit) => !listed.has(commit));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [clientsFile, commitsFile] = process.argv.slice(2);
  if (!clientsFile || !commitsFile) {
    console.error('usage: node clients-missing.mjs <clients.txt> <commits file>');
    process.exit(2);
  }
  const commits = readFileSync(commitsFile, 'utf8').split('\n').map((line) => line.trim()).filter(Boolean);
  for (const commit of commits) if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error(`${commitsFile}: ${commit} is not a 40-hex commit`);
  console.log(missingClients(readFileSync(clientsFile, 'utf8'), commits).join(' '));
}
