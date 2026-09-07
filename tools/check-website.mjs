// Dependency-free local integrity checks. Rendered and interactive QA is separate.
import { readFileSync, existsSync, statSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const site = resolve(root, 'website');
let links = 0;
for (const page of ['index.html', 'docs.html', 'demo.html']) {
  const html = readFileSync(resolve(site, page), 'utf8');
  assert(!html.includes('api.example.test'), 'The starter must use a real API');
  assert(!html.includes('data-preview-panel'), 'Do not reconstruct the native GUI');
  assert.equal([...html.matchAll(/<h1[ >]/g)].length, 1, `${page}: one main heading`);
  const ids = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
  assert.equal(ids.length, new Set(ids).size, `${page}: unique IDs`);
  for (const [, target] of html.matchAll(/data-copy-target="([^"]+)"/g)) {
    assert(ids.includes(target), `${page}: missing copy source ${target}`);
  }
  for (const [, value] of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    if (/^https?:/.test(value)) {
      const sourcePrefix = 'https://github.com/OthmaneBlial/Postly/blob/main/';
      if (value.startsWith(sourcePrefix)) {
        assert(existsSync(resolve(root, value.slice(sourcePrefix.length))), value);
      }
      continue;
    }
    assert(!value.startsWith('/'), `${page}: root-relative URL breaks /Postly/`);
    const [file, fragment] = value.split('#');
    const path = resolve(site, file || page);
    assert(existsSync(path), `${page}: missing asset ${value}`);
    assert(statSync(path).size > 0, `${page}: empty asset ${value}`);
    if (fragment) {
      assert(readFileSync(path, 'utf8').includes(`id="${fragment}"`), `${page}: missing anchor ${value}`);
    }
    links += 1;
  }
}
for (const [, asset] of readFileSync(resolve(site, 'styles.css'), 'utf8').matchAll(/url\("([^"]+)"\)/g)) {
  assert(existsSync(resolve(site, asset)), `missing stylesheet asset ${asset}`);
}
console.log(`Website integrity passed: three pages, ${links} local URLs, copy targets and font assets.`);
