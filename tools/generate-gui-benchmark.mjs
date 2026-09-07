// Fictional, deterministic workspace for native GUI measurement, never user data.
import { mkdir, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

const destination = process.argv[2];
if (!destination || process.argv.length !== 3) {
  throw new Error('Usage: node tools/generate-gui-benchmark.mjs EMPTY_DIRECTORY');
}
const root = path.resolve(destination);
await mkdir(root, { recursive: true });
if ((await readdir(root)).length) throw new Error('Destination must be empty');
const collection = path.join(root, 'collections', 'benchmark');
const requests = path.join(collection, 'requests');
await mkdir(requests, { recursive: true });
await mkdir(path.join(root, 'environments'));
await writeFile(path.join(root, 'postly.toml'), 'format = "postly"\nversion = 1\nname = "GUI benchmark — fictional data"\n', { flag: 'wx' });
await writeFile(path.join(collection, 'postly.collection.toml'), 'id = "00000000-0000-4000-8000-000000000000"\nname = "10,000 requests"\n\n[auth]\ntype = "none"\n', { flag: 'wx' });
for (let index = 0; index < 10_000; index++) {
  const number = String(index).padStart(5, '0');
  const name = index === 9999 ? 'Last request' : `Request ${number}`;
  const text = `id = "00000000-0000-4000-8000-${(index + 1).toString(16).padStart(12, '0')}"\nname = "${name}"\nmethod = "GET"\nurl = "http://127.0.0.1:1/request/${number}"\n\n[body]\ntype = "none"\n\n[auth]\ntype = "none"\n`;
  await writeFile(path.join(requests, `${number}.postly.toml`), text, { flag: 'wx' });
}
console.log(JSON.stringify({ workspace: root, requests: 10000, search: '09999', result: 'Last request', networkRequests: 0 }));
