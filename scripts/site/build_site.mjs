#!/usr/bin/env node
// Prerender the landing site: one static page per locale, plus the files
// crawlers and AI answer engines look for.
//
// The site ships six locales, but every translation lived in `app.js` and was
// swapped in at runtime, so five of them had no URL to rank and none of the
// 64 KB Q&A reached a crawler that does not run JavaScript. This turns the
// same data into real documents without changing how the page behaves for a
// human: `app.js` still owns the switcher, it just navigates now.
//
// Usage: node scripts/site/build_site.mjs [--check]
//   --check  fail if the tree is not already up to date (CI guard)

import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createContext, runInContext } from 'node:vm';
import { execFileSync } from 'node:child_process';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const SITE = join(ROOT, 'docs/ecp-landing');
const CHECK = process.argv.includes('--check');
// Star count comes from the environment, never from a live fetch: a build that
// reaches the network produces different output every time the count moves,
// which would make --check fail on a tree that is in fact current. The deploy
// job supplies it; a local build simply omits the field.
const STARS = Number(process.env.ECP_STARS) || 0;
// Freshness is a citation signal. Sourced from the newest git commit so a
// rebuild without content changes does not claim the page is newer — pinned to
// UTC because `%cs` renders in the local zone, which made the same commit
// produce a different date here than on a CI runner and failed --check.
const BUILD_DATE = execFileSync(
  'git',
  ['log', '-1', '--format=%cd', '--date=format-local:%Y-%m-%d'],
  { cwd: ROOT, env: { ...process.env, TZ: 'UTC0' } },
)
  .toString()
  .trim();

// When the count was read. Without it the number reads as "stars right now",
// which it is not — it is a snapshot taken at deploy time and frozen until the
// next release. Supplied alongside the count so both describe one moment.
const STARS_AT = /^\d{4}-\d{2}-\d{2}$/.test(process.env.ECP_STARS_AT ?? '')
  ? process.env.ECP_STARS_AT
  : BUILD_DATE;

const seo = JSON.parse(readFileSync(join(SITE, 'seo.json'), 'utf8'));
const compare = JSON.parse(readFileSync(join(SITE, 'compare.json'), 'utf8'));
const BASE = seo.baseUrl;
const OG_LOCALE = {
  en: 'en_US',
  'zh-TW': 'zh_TW',
  'zh-CN': 'zh_CN',
  ja: 'ja_JP',
  ko: 'ko_KR',
  es: 'es_ES',
};

// ── source data ──────────────────────────────────────────────────────────────

/** Workspace version — the single source of truth the hero used to drift from. */
function workspaceVersion() {
  const toml = readFileSync(join(ROOT, 'Cargo.toml'), 'utf8');
  const pkg = toml.split('[workspace.package]')[1] ?? '';
  const m = pkg.match(/^\s*version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error('cannot read [workspace.package] version from Cargo.toml');
  return m[1];
}

/**
 * `app.js` and `qa_data.js` are browser scripts. Their DOM work is inside a
 * DOMContentLoaded listener, so a stub window is enough to reach the data —
 * which keeps this generator reading the same literals the page ships rather
 * than a copy that can drift.
 */
function loadSiteData() {
  const sandbox = {
    window: {},
    document: { addEventListener() {}, querySelectorAll: () => [], getElementById: () => null },
    navigator: { language: 'en' },
  };
  sandbox.window.document = sandbox.document;
  const ctx = createContext(sandbox);
  runInContext(readFileSync(join(SITE, 'js/qa_data.js'), 'utf8'), ctx);
  runInContext(
    readFileSync(join(SITE, 'js/app.js'), 'utf8') +
      ';globalThis.__T = TRANSLATIONS; globalThis.__L = LOCALES;',
    ctx,
  );
  return { translations: ctx.__T, locales: ctx.__L, qas: sandbox.window.INTERVIEW_QAS };
}

// ── html rewriting ───────────────────────────────────────────────────────────

const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' };
const escapeHtml = (s) => String(s).replace(/[&<>"]/g, (c) => ESCAPES[c]);
/**
 * Plain text from a fragment of HTML.
 *
 * Runs to a fixed point rather than once: removing an inner tag can splice its
 * neighbours into a new one (`<scr<b>ipt>` becomes `<script>` after a single
 * pass), so one replace leaves exactly the markup it was meant to remove.
 */
const stripTags = (s) => {
  let out = String(s);
  for (let prev = null; prev !== out; ) {
    prev = out;
    out = out.replace(/<[^>]*>/g, '');
  }
  return out.replace(/\s+/g, ' ').trim();
};

/**
 * JSON for embedding inside a `<script>` element. `</script>` anywhere in a
 * string would close the element early and drop the rest of the page into
 * markup, so `<` never reaches the document as itself. Escaping here rather
 * than trusting the text to be clean keeps the guarantee independent of what
 * the Q&A file happens to contain.
 */
const scriptJson = (value) => JSON.stringify(value).replace(/</g, '\\u003c');

/** End index of the element opened at `openEnd`, counting nested same-name tags. */
function closingIndex(html, tag, openEnd) {
  const open = new RegExp(`<${tag}(?=[\\s/>])`, 'gi');
  const close = new RegExp(`</${tag}\\s*>`, 'gi');
  let depth = 1;
  let cursor = openEnd;
  for (;;) {
    open.lastIndex = cursor;
    close.lastIndex = cursor;
    const o = open.exec(html);
    const c = close.exec(html);
    if (!c) return -1;
    if (o && o.index < c.index) {
      depth += 1;
      cursor = o.index + o[0].length;
      continue;
    }
    depth -= 1;
    if (depth === 0) return c.index;
    cursor = c.index + c[0].length;
  }
}

/** Replace the inner HTML of every `[data-i18n]` element with its translation. */
function applyTranslations(html, dict) {
  const marker = /<([a-z0-9]+)([^>]*?)\sdata-i18n="([^"]+)"([^>]*)>/gi;
  let out = '';
  let last = 0;
  for (;;) {
    marker.lastIndex = last;
    const m = marker.exec(html);
    if (!m) break;
    const [full, tag, , key] = m;
    const openEnd = m.index + full.length;
    const close = closingIndex(html, tag, openEnd);
    if (close < 0 || dict[key] === undefined) {
      out += html.slice(last, openEnd);
      last = openEnd;
      continue;
    }
    out += html.slice(last, openEnd) + dict[key];
    last = close;
  }
  return out + html.slice(last);
}

function replaceRegion(html, name, body) {
  const open = `<!-- ${name}:start -->`;
  const close = `<!-- ${name}:end -->`;
  const a = html.indexOf(open);
  const b = html.indexOf(close);
  if (a < 0 || b < 0) throw new Error(`missing region markers for ${name}`);
  return html.slice(0, a + open.length) + body + html.slice(b);
}

// ── generated fragments ──────────────────────────────────────────────────────

function faqHtml(qas) {
  // Mirrors the markup `app.js` renders on a language switch, so the static
  // page and the switched page are styled by the same rules and the crawler
  // sees exactly what a reader sees.
  const icon =
    '<svg class="acc-icon" width="20" height="20" viewBox="0 0 24 24" fill="none" ' +
    'stroke="currentColor" stroke-width="2"><line x1="12" y1="5" x2="12" y2="19"></line>' +
    '<line x1="5" y1="12" x2="19" y2="12"></line></svg>';
  return qas
    .map(
      (qa) => `
                        <div class="acc-item">
                            <button class="acc-trigger">
                                <span class="acc-q">${qa.q}</span>
                                ${icon}
                            </button>
                            <div class="acc-content">
                                <div class="acc-inner">${qa.a}</div>
                            </div>
                        </div>`,
    )
    .join('');
}

function headHtml(locale, meta, version, qas, qaLocale) {
  const path = seo.localePaths[locale];
  const canonical = BASE + path;
  const alternates = Object.entries(seo.localePaths)
    .map(
      ([code, p]) =>
        `    <link rel="alternate" hreflang="${code}" href="${BASE}${p}">`,
    )
    .join('\n');

  // Answer engines lift Q&A pairs verbatim; give them the same text the page
  // renders, in this locale, rather than making them infer it from prose.
  // `qaLocale` is the language the answers are actually written in, which is
  // not always this page's locale: qa_data.js carries the English text under
  // ja / ko / es. Declaring inLanguage: "ja" over English prose would be a
  // false claim in the one place a machine is most likely to believe it.
  const faqLd = {
    '@context': 'https://schema.org',
    '@type': 'FAQPage',
    inLanguage: qaLocale,
    mainEntity: qas.map((qa) => ({
      '@type': 'Question',
      name: stripTags(qa.q),
      acceptedAnswer: { '@type': 'Answer', text: stripTags(qa.a) },
    })),
  };

  const appLd = {
    '@context': 'https://schema.org',
    '@type': 'SoftwareApplication',
    name: 'Egent Code Plexus',
    alternateName: 'ecp',
    applicationCategory: 'DeveloperApplication',
    applicationSubCategory: 'Code intelligence graph',
    operatingSystem: 'Linux, macOS, Windows',
    softwareVersion: version,
    inLanguage: locale,
    url: canonical,
    codeRepository: seo.repoUrl,
    downloadUrl: `${seo.repoUrl}/releases/latest`,
    license: 'https://opensource.org/licenses/MIT',
    programmingLanguage: 'Rust',
    description: meta.description,
    sameAs: seo.sameAs,
    dateModified: BUILD_DATE,
    // GitHub stars are a bookmark count, not a rating: there is no scale and
    // no negative pole, so `aggregateRating` cannot be derived from them
    // without inventing a score — the exact fabrication Google penalises, and
    // a poor look for a tool whose pitch is that it says "I don't know".
    // `interactionStatistic` states the count as the fact it is.
    ...(STARS
      ? {
          interactionStatistic: {
            '@type': 'InteractionCounter',
            interactionType: 'https://schema.org/LikeAction',
            userInteractionCount: STARS,
            endTime: STARS_AT,
          },
        }
      : {}),
    offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
    featureList: [
      'Callers and callees of any symbol across 31 languages',
      'Blast radius of a diff before the refactor lands',
      'HTTP routes and cross-repo contract extraction',
      'SQL tables reached from code, including raw query strings',
      'BlindSpot records naming what the graph cannot see',
    ],
  };

  const siteLd = {
    '@context': 'https://schema.org',
    '@type': 'WebSite',
    name: 'Egent Code Plexus',
    url: canonical,
    inLanguage: locale,
    description: meta.description,
    sameAs: seo.sameAs,
  };

  // The install block is the thing a reader is most likely to ask an assistant
  // for; give it as steps rather than leaving it inside a tab.
  const howToLd = {
    '@context': 'https://schema.org',
    '@type': 'HowTo',
    name: 'Install Egent Code Plexus and index a repository',
    inLanguage: locale,
    step: [
      {
        '@type': 'HowToStep',
        name: 'Install the binary',
        text: `curl -sSfL ${seo.repoUrl}/releases/latest/download/install.sh | sh`,
      },
      { '@type': 'HowToStep', name: 'Index a repository', text: 'ecp admin index --repo .' },
      {
        '@type': 'HowToStep',
        name: 'Ask a structural question',
        text: 'ecp impact --target <symbol> --direction upstream',
      },
    ],
  };

  return `
    <title>${escapeHtml(meta.title)}</title>
    <meta name="description" content="${escapeHtml(meta.description)}">
    <link rel="canonical" href="${canonical}">
${alternates}
    <link rel="alternate" hreflang="x-default" href="${BASE}">
    <meta name="robots" content="index, follow, max-snippet:-1, max-image-preview:large">
    <link rel="icon" href="${BASE}favicon.svg" type="image/svg+xml">
    <link rel="alternate icon" href="${BASE}favicon.ico" sizes="32x32">
    <link rel="apple-touch-icon" href="${BASE}apple-touch-icon.png">
    <meta name="theme-color" content="#000000">
    <meta property="og:site_name" content="Egent Code Plexus">
    <meta property="og:locale" content="${OG_LOCALE[locale]}">
    <meta property="og:title" content="${escapeHtml(meta.ogTitle)}">
    <meta property="og:description" content="${escapeHtml(meta.ogDescription)}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="${canonical}">
    <meta property="og:image" content="${BASE}og.png">
    <meta property="og:image:width" content="1200">
    <meta property="og:image:height" content="630">
    <meta property="og:image:alt" content="Egent Code Plexus — built for agents, not IDEs">
    <meta name="twitter:card" content="summary_large_image">
    <meta name="twitter:title" content="${escapeHtml(meta.ogTitle)}">
    <meta name="twitter:description" content="${escapeHtml(meta.ogDescription)}">
    <meta name="twitter:image" content="${BASE}og.png">
    <script type="application/ld+json">${scriptJson(appLd)}</script>
    <script type="application/ld+json">${scriptJson(faqLd)}</script>
    <script type="application/ld+json">${scriptJson(siteLd)}</script>
    <script type="application/ld+json">${scriptJson(howToLd)}</script>
`;
}

/** Slice an element out of the template so both pages share one chrome. */
function templateElement(tag) {
  const open = template.search(new RegExp(`<${tag}[\\s>]`));
  if (open < 0) throw new Error(`template has no <${tag}>`);
  const openEnd = template.indexOf('>', open) + 1;
  const close = closingIndex(template, tag, openEnd);
  return template.slice(open, close + `</${tag}>`.length);
}

// ── comparison page ──────────────────────────────────────────────────────────

/**
 * "Which of these should I use" is the question an engine gets asked, and it
 * is answered from whatever page carries the numbers. This one carries them
 * with the conditions attached — versions, hardware, and an explicit section
 * on what the measurements do not cover, because a comparison a reader cannot
 * check is worth less than no comparison at all.
 */
function comparePage(locale, version) {
  const t = compare.i18n[locale] ?? compare.i18n[seo.defaultLocale];
  const path = `${seo.localePaths[locale]}compare/`;
  const canonical = BASE + path;
  const up = '../'.repeat(path.split('/').filter(Boolean).length);

  const head = [
    `<title>${escapeHtml(t.title)} — ${escapeHtml(t.subtitle)}</title>`,
    `<meta name="description" content="${escapeHtml(stripTags(t.intro)).slice(0, 300)}">`,
    `<link rel="canonical" href="${canonical}">`,
    ...Object.entries(seo.localePaths).map(
      ([c, p2]) => `<link rel="alternate" hreflang="${c}" href="${BASE}${p2}compare/">`,
    ),
    `<link rel="alternate" hreflang="x-default" href="${BASE}compare/">`,
    '<meta name="robots" content="index, follow, max-snippet:-1, max-image-preview:large">',
    `<link rel="icon" href="${BASE}favicon.svg" type="image/svg+xml">`,
    `<meta property="og:title" content="${escapeHtml(t.title)}">`,
    `<meta property="og:description" content="${escapeHtml(t.subtitle)}">`,
    `<meta property="og:url" content="${canonical}">`,
    `<meta property="og:image" content="${BASE}og.png">`,
    '<meta name="twitter:card" content="summary_large_image">',
  ].join('\n    ');

  const articleLd = {
    '@context': 'https://schema.org',
    '@type': 'TechArticle',
    headline: t.title,
    description: stripTags(t.subtitle),
    inLanguage: locale,
    url: canonical,
    dateModified: BUILD_DATE,
    about: compare.tools.map((tool) => ({
      '@type': 'SoftwareApplication',
      name: tool.name,
      applicationCategory: 'DeveloperApplication',
      url: tool.url,
    })),
    isBasedOn: `${seo.repoUrl}#-performance-receipts`,
  };

  const headers = compare.tools
    .map((tool) => `<th><a href="${tool.url}" rel="noopener">${tool.name}</a><br><span class="mono compare-stack">${tool.stack}</span></th>`)
    .join('');

  const tables = compare.suites
    .map((suite) => {
      const rows = suite.rows
        .map((r) => {
          const cells = r.values
            .map((v, i) => `<td${i === r.best ? ' class="compare-best"' : ''}>${escapeHtml(v)}</td>`)
            .join('');
          return `<tr><td>${escapeHtml(r.metric)}</td>${cells}</tr>`;
        })
        .join('\n                    ');
      const note = suite.footnote
        ? `<p class="table-caption">${escapeHtml(suite.footnote)}</p>`
        : '';
      return `
            <h3 class="mono accent">${escapeHtml(suite.repo)}</h3>
            <p class="compare-shape">${escapeHtml(suite.shape)}</p>
            <div class="table-container">
                <table class="saas-table">
                    <thead><tr><th>Metric</th>${headers}</tr></thead>
                    <tbody>
                    ${rows}
                    </tbody>
                </table>
            </div>
            ${note}`;
    })
    .join('\n');

  // The chrome is the landing page's own markup, fonts included, so the two
  // pages cannot drift apart visually. app.js runs here too: it reads the
  // locale and page from the globals below, so the language selector works and
  // switching language stays on the comparison page instead of dropping the
  // reader on the home page of another locale.
  const chrome = {
    header: applyTranslations(templateElement('header'), compare.dict ?? {}),
    footer: templateElement('footer'),
  };

  return `<!DOCTYPE html>
<html lang="${locale}">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    ${head}
    <link rel="stylesheet" href="${up}css/style.css">
    <link rel="preconnect" href="https://fonts.googleapis.com">
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
    <link href="https://fonts.googleapis.com/css2?family=Fira+Code:wght@400;500;700&family=Inter:wght@400;500;600;800&display=swap" rel="stylesheet">
    <script type="application/ld+json">${scriptJson(articleLd)}</script>
</head>
<body class="theme-dark">
${chrome.header}

    <section class="hero compare-hero">
        <div class="hero-bg-glow"></div>
        <div class="container hero-content">
            <h1 class="hero-title">${escapeHtml(t.title)}</h1>
            <p class="hero-tagline mono">${escapeHtml(t.subtitle)}</p>
            <p class="hero-answer">${t.intro}</p>
        </div>
    </section>

    <main class="container compare-main">
        <h2 class="section-heading">${escapeHtml(t.methodologyHeading)}</h2>
        <ul class="compare-method">
            <li><strong>Versions</strong> — ${escapeHtml(compare.methodology.versions)}</li>
            <li><strong>Hardware</strong> — ${escapeHtml(compare.methodology.hardware)}</li>
            <li>${escapeHtml(compare.methodology.protocol)}</li>
        </ul>
${tables}

        <h2 class="section-heading">${escapeHtml(t.limitsHeading)}</h2>
        <p class="hero-answer compare-limits">${t.limits}</p>
        <p class="table-caption">${escapeHtml(t.footer.replace('{version}', version))}</p>
        <p class="compare-back"><a href="../">&larr; ${escapeHtml(seo.meta[locale].ogTitle)}</a></p>
    </main>

${chrome.footer}
    <script>window.__ECP_LOCALE__ = "${locale}"; window.__ECP_ROOT__ = "${up}"; window.__ECP_PAGE__ = "compare/";</script>
    <script src="${up}js/qa_data.js"></script>
    <script src="${up}js/app.js"></script>
</body>
</html>
`;
}

// ── crawler files ────────────────────────────────────────────────────────────

function sitemapXml() {
  const today = BUILD_DATE;
  const pages = Object.entries(seo.localePaths).flatMap(([code, p]) => [
    { code, p, suffix: '' },
    { code, p, suffix: 'compare/' },
  ]);
  const urls = pages
    .map(({ code, p, suffix }) => {
      const links = Object.entries(seo.localePaths)
        .map(
          ([c, q]) =>
            `        <xhtml:link rel="alternate" hreflang="${c}" href="${BASE}${q}${suffix}"/>`,
        )
        .join('\n');
      return `    <url>
        <loc>${BASE}${p}${suffix}</loc>
        <lastmod>${today}</lastmod>
        <changefreq>weekly</changefreq>
        <priority>${code === seo.defaultLocale ? (suffix ? '0.9' : '1.0') : '0.8'}</priority>
${links}
        <xhtml:link rel="alternate" hreflang="x-default" href="${BASE}${suffix}"/>
    </url>`;
    })
    .join('\n');
  return `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">
${urls}
</urlset>
`;
}

function robotsTxt() {
  // Answer engines are the audience this site is written for, so say yes by
  // name rather than relying on the wildcard: Google-Extended and
  // Applebot-Extended in particular are read as opt-OUT switches, and a site
  // that never mentions them is easy to treat as undecided.
  const agents = [
    'GPTBot', 'OAI-SearchBot', 'ChatGPT-User',
    'ClaudeBot', 'Claude-User', 'Claude-SearchBot', 'anthropic-ai',
    'PerplexityBot', 'Perplexity-User',
    'Google-Extended', 'Applebot-Extended', 'Bingbot',
    'CCBot', 'cohere-ai', 'Meta-ExternalAgent', 'Amazonbot', 'DuckAssistBot',
  ];
  const blocks = agents.map((a) => `User-agent: ${a}\nAllow: /\n`).join('\n');
  return `User-agent: *
Allow: /

${blocks}
Sitemap: ${BASE}sitemap.xml

# Plain-text summaries for language models:
# ${BASE}llms.txt
# ${BASE}llms-full.txt
`;
}

/**
 * `llms.txt` is what an answer engine reads when it wants the plain-text
 * version of a site. Written as claims with their measurement conditions
 * attached, because an unqualified number is the thing a model repeats wrong.
 */
function llmsTxt(version, qas) {
  const faq = qas
    .slice(0, 8)
    .map((qa) => `### ${stripTags(qa.q)}\n\n${stripTags(qa.a)}`)
    .join('\n\n');
  return `# Egent Code Plexus (ecp)

> A code intelligence graph built for AI coding agents rather than for humans
> reading an IDE. It answers structural questions — who calls this symbol, what
> does this change break, which routes and database tables does this service
> reach — over 31 languages in one graph.

Version: ${version}
Repository: ${seo.repoUrl}
License: MIT
Install: curl -sSfL ${seo.repoUrl}/releases/latest/download/install.sh | sh

## What it is for

An agent editing code needs to know what its edit touches before it makes the
edit. Text search answers "where does this string appear"; ecp answers "what
depends on this declaration", which is a different question and the one that
decides whether a refactor is safe.

## What it does

- \`ecp find <name>\` — locate a declaration by symbol, not by string match.
- \`ecp impact --target <symbol> --direction upstream\` — callers of a symbol.
- \`ecp impact --baseline <ref>\` — blast radius of a diff against a git ref.
- \`ecp routes\` / \`ecp contracts\` — HTTP routes and cross-repo API contracts.
- \`ecp cypher '<query>'\` — arbitrary graph queries across indexed repos.
- \`ecp peers\` — collision detection between concurrent agents in one repo.

## Measured claims

Each number below is from the project's own benchmark on an AMD Ryzen 9 9950X,
39.2 GiB RAM, Linux 6.6.87. Numbers on other hardware and other corpora differ.

- Token cost: a 3-hop call chain from \`ecp impact\` is about 111 tokens; the
  equivalent grep output is about 830, before counting the files an agent must
  then open to interpret it.
- Query latency: structural queries target under 100 ms on a warm index.
- Languages: 31 parsed, of which 14 are covered by the mainstream test matrix.

## Honesty model

Where the graph cannot see something it emits a BlindSpot record naming the
gap, rather than an empty result that reads as "nothing exists". Known gaps are
deliberate and documented: function-body locals are dropped, and a handful of
language constructs are unsupported per language. A tool that hides its blind
spots teaches a model to trust an answer that was never there.

## How it compares

${compareText()}

## Questions and answers

${faq}
`;
}

/**
 * The comparison, in the form a model will repeat it. Conditions come first
 * and stay attached to the numbers: an unqualified "15x faster" is exactly the
 * claim that gets restated without its corpus, its versions or its hardware.
 */
function compareText() {
  const m = compare.methodology;
  const names = compare.tools.map((t) => `${t.name} (${t.stack})`).join(', ');
  const suites = compare.suites
    .map((suite) => {
      const rows = suite.rows
        .map((r) => `| ${r.metric} | ${r.values.join(' | ')} |`)
        .join('\n');
      const note = suite.footnote ? `\n\n${suite.footnote}` : '';
      return `### ${suite.repo} — ${suite.shape}

| Metric | ${compare.tools.map((t) => t.name).join(' | ')} |
|---|${compare.tools.map(() => '---').join('|')}|
${rows}${note}`;
    })
    .join('\n\n');
  return `Measured against ${names} on the same checkouts and the same machine.
Versions: ${m.versions}. Hardware: ${m.hardware}. ${m.protocol}

${suites}

These numbers are one machine, one set of versions, two corpora. They do not
say which tool has the API a given project wants, whether its language
coverage matches that stack, or how any of them behave on a differently shaped
repository. Re-measure before deciding. Full page: ${BASE}compare/`;
}

/**
 * Everything an answer engine could want in one fetch: the summary above plus
 * every Q&A pair in every language, unabridged. `llms.txt` is the index;
 * this is the corpus.
 */
function llmsFull(version, allQas) {
  const sections = Object.keys(seo.localePaths)
    .map((locale) => {
      const rows = (allQas[locale] ?? [])
        .map((qa) => `### ${stripTags(qa.q)}\n\n${stripTags(qa.a)}`)
        .join('\n\n');
      return `## Questions and answers (${locale})\n\nCanonical page: ${BASE}${seo.localePaths[locale]}\n\n${rows}`;
    })
    .join('\n\n');
  return `${llmsTxt(version, allQas[seo.defaultLocale])}\n\n---\n\n${sections}\n`;
}

// ── build ────────────────────────────────────────────────────────────────────

const version = workspaceVersion();
const { translations, qas } = loadSiteData();
const template = readFileSync(join(SITE, 'template.html'), 'utf8');
const written = [];
let stale = [];

function emit(relPath, content) {
  const target = join(SITE, relPath);
  mkdirSync(dirname(target), { recursive: true });
  const current = existsSync(target) ? readFileSync(target, 'utf8') : null;
  if (current !== content) {
    if (CHECK) stale.push(relPath);
    else writeFileSync(target, content);
  }
  written.push(relPath);
}

for (const [locale, path] of Object.entries(seo.localePaths)) {
  const dict = translations[locale] ?? {};
  const localeQas = qas[locale] ?? qas[seo.defaultLocale];
  // Identity against the English set is the only honest test available: these
  // locales ship the English answers verbatim rather than a translation.
  const qaLocale =
    locale !== seo.defaultLocale &&
    JSON.stringify(localeQas) === JSON.stringify(qas[seo.defaultLocale])
      ? seo.defaultLocale
      : locale;
  const meta = seo.meta[locale];
  const depth = path ? '../' : '';

  let html = applyTranslations(template, dict);
  html = replaceRegion(html, 'head', headHtml(locale, meta, version, localeQas, qaLocale));
  html = replaceRegion(html, 'faq', faqHtml(localeQas));
  html = replaceRegion(html, 'answer', seo.answer[locale]);
  html = html
    .replace('<html lang="en">', `<html lang="${locale}">`)
    .replace(/\{\{VERSION\}\}/g, version)
    .replace(/\{\{LOCALE\}\}/g, locale)
    .replace(/\{\{ASSET_PREFIX\}\}/g, depth)
    .replace(/\{\{LOCALE_PATH\}\}/g, path);
  emit(`${path}index.html`, html);
  emit(`${path}compare/index.html`, comparePage(locale, version));
}

// IndexNow verification. The key lives at the site's own path rather than the
// host root, because the host root is a different repository — which is why
// every submission has to carry `keyLocation` and can only vouch for URLs
// under this prefix.
if (seo.indexNowKey) emit(`${seo.indexNowKey}.txt`, seo.indexNowKey);
emit('sitemap.xml', sitemapXml());
emit('robots.txt', robotsTxt());
emit('llms.txt', llmsTxt(version, qas[seo.defaultLocale]));
emit('llms-full.txt', llmsFull(version, qas));
for (const [locale, path] of Object.entries(seo.localePaths)) {
  if (!path) continue;
  emit(`${path}llms.txt`, llmsTxt(version, qas[locale] ?? qas[seo.defaultLocale]));
}

if (CHECK && stale.length) {
  console.error(`site is stale, re-run scripts/site/build_site.mjs:\n  ${stale.join('\n  ')}`);
  process.exit(1);
}
console.log(`built ${written.length} files for v${version}: ${written.join(', ')}`);
