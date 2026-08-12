#!/usr/bin/env node
// Per-page `lastmod`, stamped into the deployed sitemap.
//
// Two things stop the generator from doing this itself.
//
// A date derived from git cannot be committed: the value for a page would be
// the date of the commit that contains it, which does not exist yet when the
// file is written, so `--check` compares a stale artifact against a fresh
// computation and fails on a tree nobody touched. This runs at deploy instead,
// on the tree about to be published, which has no such circularity — the same
// seam `ECP_STARS` already uses.
//
// And a naive "did the file change" test answers the wrong question. Every page
// carries the version string, so every page's bytes change on every release:
// the v0.9.3 deploy rewrote 19 files and 25 lines, all of them `0.9.2` becoming
// `0.9.3`. Dates derived from that would be one date for every page, which is
// what a single `contentDate` already says.
//
// So the comparison uses `.page-hashes.json`, which the generator writes by
// rendering every page a second time with a sentinel version. Only the
// generator knows where it interpolated the version; searching the output for
// the version string instead would eat `codegraph 0.9.4` out of the comparison
// methodology the moment ecp itself reaches 0.9.4.
//
// State lives in `.lastmod.json` in the deployed tree, which is why the deploy
// rsync excludes it. Losing the file is not a failure: every page then gets
// today, and differentiation returns on the next release.
//
// Usage: node scripts/site/stamp_lastmod.mjs <site-dir> <YYYY-MM-DD>

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const [siteDir, today] = process.argv.slice(2);
if (!siteDir || !/^\d{4}-\d{2}-\d{2}$/.test(today ?? '')) {
  throw new Error('usage: stamp_lastmod.mjs <site-dir> <YYYY-MM-DD>');
}

const STATE = join(siteDir, '.lastmod.json');
const hashes = JSON.parse(readFileSync(join(siteDir, '.page-hashes.json'), 'utf8'));
const sitemapPath = join(siteDir, 'sitemap.xml');
const sitemap = readFileSync(sitemapPath, 'utf8');
const previous = existsSync(STATE) ? JSON.parse(readFileSync(STATE, 'utf8')) : {};
const state = {};

let carried = 0;
let stamped = 0;
const updated = sitemap.replace(
  /<loc>([^<]+)<\/loc>\s*<lastmod>[^<]*<\/lastmod>/g,
  (whole, loc) => {
    const hash = hashes[loc];
    if (!hash) throw new Error(`sitemap lists ${loc}, which .page-hashes.json does not cover`);
    const unchanged = previous[loc]?.hash === hash;
    const date = unchanged ? previous[loc].date : today;
    if (unchanged) carried += 1;
    else stamped += 1;
    state[loc] = { hash, date };
    return whole.replace(/<lastmod>[^<]*<\/lastmod>/, `<lastmod>${date}</lastmod>`);
  },
);

writeFileSync(sitemapPath, updated);
writeFileSync(STATE, `${JSON.stringify(state, null, 2)}\n`);
console.log(`lastmod: ${stamped} pages stamped ${today}, ${carried} carried forward`);
