#!/usr/bin/env node
// Submit every URL in the deployed sitemap to IndexNow.
//
// The key file has been served since the site gained one, so verification was
// in place for submissions nobody was making. This closes that half: one
// request per deploy, listing exactly what the sitemap lists.
//
// `keyLocation` is mandatory here because the key sits under the project path
// rather than at the host root — the host root is a different repository — so
// a submission can only vouch for URLs under this prefix.
//
// Usage: node scripts/site/indexnow.mjs <deployed-site-dir> [--dry-run]

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const ENDPOINT = 'https://api.indexnow.org/IndexNow';
const DRY_RUN = process.argv.includes('--dry-run');
const siteDir = process.argv[2] && !process.argv[2].startsWith('--')
  ? process.argv[2]
  : join(ROOT, 'docs/ecp-landing');

const seo = JSON.parse(readFileSync(join(ROOT, 'docs/ecp-landing/seo.json'), 'utf8'));
if (!seo.indexNowKey) {
  console.log('seo.json has no indexNowKey; nothing to submit.');
  process.exit(0);
}

const sitemap = readFileSync(join(siteDir, 'sitemap.xml'), 'utf8');
const urlList = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1].trim());
if (urlList.length === 0) throw new Error(`no <loc> entries in ${siteDir}/sitemap.xml`);

// A submission is rejected wholesale when one URL is off-host or outside the
// key's directory, so the mismatch is worth naming here rather than reading it
// back out of a 422.
const outside = urlList.filter((url) => !url.startsWith(seo.baseUrl));
if (outside.length > 0) throw new Error(`sitemap URLs outside ${seo.baseUrl}: ${outside.join(', ')}`);

const payload = {
  host: new URL(seo.baseUrl).host,
  key: seo.indexNowKey,
  keyLocation: `${seo.baseUrl}${seo.indexNowKey}.txt`,
  urlList,
};

console.log(`submitting ${urlList.length} urls as ${payload.host}`);
if (DRY_RUN) {
  console.log(JSON.stringify(payload, null, 2));
  process.exit(0);
}

const response = await fetch(ENDPOINT, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json; charset=utf-8' },
  body: JSON.stringify(payload),
});
console.log(`IndexNow responded ${response.status}: ${(await response.text()).trim() || '(empty)'}`);
