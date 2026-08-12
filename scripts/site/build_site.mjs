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

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const SITE = join(ROOT, 'docs/ecp-landing');
const CHECK = process.argv.includes('--check');

const seo = JSON.parse(readFileSync(join(SITE, 'seo.json'), 'utf8'));
const compare = JSON.parse(readFileSync(join(SITE, 'compare.json'), 'utf8'));
const integrations = JSON.parse(readFileSync(join(SITE, 'integrations.json'), 'utf8'));

// Freshness is a citation signal, and it is committed rather than computed.
// Deriving it from git put a clock in the build: CI checks out the synthetic
// merge commit GitHub creates for a pull request, whose date is the moment the
// check runs, so an unchanged tree went stale the moment the UTC day rolled
// over. A value in seo.json is the same for every builder on every day —
// `cut-release.sh` advances it when a release ships.
const BUILD_DATE = seo.contentDate;
if (!/^\d{4}-\d{2}-\d{2}$/.test(BUILD_DATE ?? '')) {
  throw new Error('seo.json needs contentDate as YYYY-MM-DD');
}

// Star count comes from the environment, never from a live fetch: a build that
// reaches the network produces different output every time the count moves,
// which would make --check fail on a tree that is in fact current. The deploy
// job supplies it; a local build simply omits the field.
const STARS = Number(process.env.ECP_STARS) || 0;
// When the count was read. Without it the number reads as "stars right now",
// which it is not — it is a snapshot taken at deploy time and frozen until the
// next release. Supplied alongside the count so both describe one moment.
const STARS_AT = /^\d{4}-\d{2}-\d{2}$/.test(process.env.ECP_STARS_AT ?? '')
  ? process.env.ECP_STARS_AT
  : BUILD_DATE;

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
/** Every tool in the table except ecp itself, which is column 0 everywhere. */
const RIVALS = compare.tools.slice(1);

/**
 * One page per rival on top of the hub page that carries all of them. "ecp vs
 * <tool>" is the query people actually type and the shape an answer engine
 * quotes, and a two-column table is the thing it can lift whole; the hub's
 * four-column table answers a question nobody asked in those words.
 *
 * No new prose: the head, the intro and the methodology are the same localized
 * strings the hub uses, and the numbers are the same measurements with the
 * other tool's column dropped. A per-rival paragraph would have to be invented
 * in six languages, and an invented sentence is worth less than a URL.
 */
function comparePage(locale, version, rival = null) {
  const t = compare.i18n[locale] ?? compare.i18n[seo.defaultLocale];
  const leaf = rival ? `compare/${rival.slug}/` : 'compare/';
  const path = `${seo.localePaths[locale]}${leaf}`;
  const canonical = BASE + path;
  const up = '../'.repeat(path.split('/').filter(Boolean).length);
  const columns = rival ? [compare.tools[0], rival] : compare.tools;
  const keep = rival ? [0, compare.tools.indexOf(rival)] : compare.tools.map((_, i) => i);
  const title = rival ? `${t.title} · ecp vs ${rival.name}` : t.title;

  const head = [
    `<title>${escapeHtml(title)} — ${escapeHtml(t.subtitle)}</title>`,
    `<meta name="description" content="${escapeHtml(stripTags(t.intro)).slice(0, 300)}">`,
    `<link rel="canonical" href="${canonical}">`,
    ...Object.entries(seo.localePaths).map(
      ([c, p2]) => `<link rel="alternate" hreflang="${c}" href="${BASE}${p2}${leaf}">`,
    ),
    `<link rel="alternate" hreflang="x-default" href="${BASE}${leaf}">`,
    '<meta name="robots" content="index, follow, max-snippet:-1, max-image-preview:large">',
    `<link rel="icon" href="${BASE}favicon.svg" type="image/svg+xml">`,
    '<meta property="og:type" content="article">',
    `<meta property="og:locale" content="${OG_LOCALE[locale]}">`,
    '<meta property="og:site_name" content="Egent Code Plexus">',
    `<meta property="og:title" content="${escapeHtml(title)}">`,
    `<meta property="og:description" content="${escapeHtml(t.subtitle)}">`,
    `<meta property="og:url" content="${canonical}">`,
    `<meta property="og:image" content="${BASE}og.png">`,
    '<meta name="twitter:card" content="summary_large_image">',
  ].join('\n    ');

  const articleLd = {
    '@context': 'https://schema.org',
    '@type': 'TechArticle',
    headline: title,
    description: stripTags(t.subtitle),
    inLanguage: locale,
    url: canonical,
    dateModified: BUILD_DATE,
    about: columns.map((tool) => ({
      '@type': 'SoftwareApplication',
      name: tool.name,
      applicationCategory: 'DeveloperApplication',
      url: tool.url,
    })),
    isBasedOn: `${seo.repoUrl}#-performance-receipts`,
  };

  const headers = columns
    .map((tool) => `<th><a href="${tool.url}" rel="noopener">${tool.name}</a><br><span class="mono compare-stack">${tool.stack}</span></th>`)
    .join('');

  const tables = compare.suites
    .map((suite) => {
      const rows = suite.rows
        .map((r) => {
          // `best` indexes the full row, so it has to be remapped once a column
          // is dropped — and dropped entirely when the winner is the tool this
          // page does not show, rather than silently crowning the runner-up.
          const cells = keep
            .map((column) => `<td${column === r.best ? ' class="compare-best"' : ''}>${escapeHtml(r.values[column])}</td>`)
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

  // Tool names, not a sentence: these links carry the one phrase every locale
  // shares, so the hub reaches its per-rival pages without a translated label.
  const rivalLinks = rival
    ? ''
    : `
        <ul class="compare-method compare-rivals">
${RIVALS.map((tool) => `            <li><a href="${tool.slug}/">ecp vs ${escapeHtml(tool.name)}</a></li>`).join('\n')}
        </ul>`;

  // The chrome is the landing page's own markup, fonts included, so the two
  // pages cannot drift apart visually. app.js runs here too: it reads the
  // locale and page from the globals below, so the language selector works and
  // switching language stays on the comparison page instead of dropping the
  // reader on the home page of another locale.
  const chrome = {
    header: applyTranslations(templateElement('header'), compare.dict ?? {}),
    // The template's footer links to `compare/`, which is relative to a
    // landing page; on the comparison page itself that resolves to
    // /<locale>/compare/compare/ and 404s. This page already links home.
    footer: templateElement('footer').replace(
      /\s*<p class="footer-links">[\s\S]*?<\/p>/,
      '',
    ),
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

${rivalLinks}
        <h2 class="section-heading">${escapeHtml(t.limitsHeading)}</h2>
        <p class="hero-answer compare-limits">${t.limits}</p>
        <p class="table-caption">${escapeHtml(t.footer.replace('{version}', version))}</p>
        <p class="compare-back"><a href="${rival ? '../../' : '../'}">&larr; ${escapeHtml(seo.meta[locale].ogTitle)}</a></p>
    </main>

${chrome.footer}
    <script>window.__ECP_LOCALE__ = "${locale}"; window.__ECP_ROOT__ = "${up}"; window.__ECP_PAGE__ = "${leaf}";</script>
    <script src="${up}js/qa_data.js"></script>
    <script src="${up}js/app.js"></script>
</body>
</html>
`;
}


/**
 * One page per agent host, plus a hub carrying the channel matrix.
 *
 * "how do I give <agent> codebase awareness" is a question with a host in it,
 * and the answer is a different set of commands for each one. The hub's matrix
 * is the part worth quoting; the host pages are the part worth following.
 *
 * Every command is copied from the shipped binary's own `--help`, and a host
 * whose installer still prints a placeholder is described as manual rather
 * than supported — an install page that lies costs more than the traffic it
 * earns.
 */
function integrationPage(locale, version, host = null) {
  const t = integrations.i18n[locale] ?? integrations.i18n[seo.defaultLocale];
  const leaf = host ? `integrations/${host.slug}/` : 'integrations/';
  const path = `${seo.localePaths[locale]}${leaf}`;
  const canonical = BASE + path;
  const up = '../'.repeat(path.split('/').filter(Boolean).length);
  const copy = host ? t.hosts[host.slug] : null;
  const title = host ? `${host.name} — ${t.title}` : `${t.title} — ${seo.meta[locale].ogTitle}`;
  const description = stripTags(host ? copy.intro : t.intro).slice(0, 300);

  const head = [
    `<title>${escapeHtml(title)}</title>`,
    `<meta name="description" content="${escapeHtml(description)}">`,
    `<link rel="canonical" href="${canonical}">`,
    ...Object.entries(seo.localePaths).map(
      ([c, p2]) => `<link rel="alternate" hreflang="${c}" href="${BASE}${p2}${leaf}">`,
    ),
    `<link rel="alternate" hreflang="x-default" href="${BASE}${leaf}">`,
    '<meta name="robots" content="index, follow, max-snippet:-1, max-image-preview:large">',
    `<link rel="icon" href="${BASE}favicon.svg" type="image/svg+xml">`,
    '<meta property="og:type" content="article">',
    `<meta property="og:locale" content="${OG_LOCALE[locale]}">`,
    '<meta property="og:site_name" content="Egent Code Plexus">',
    `<meta property="og:title" content="${escapeHtml(title)}">`,
    `<meta property="og:description" content="${escapeHtml(stripTags(t.subtitle))}">`,
    `<meta property="og:url" content="${canonical}">`,
    `<meta property="og:image" content="${BASE}og.png">`,
    '<meta name="twitter:card" content="summary_large_image">',
  ].join('\n    ');

  // HowTo only where there are steps to follow. The hub is a table, and a
  // HowTo without steps is structured data describing nothing.
  const howToLd = host?.steps
    ? {
        '@context': 'https://schema.org',
        '@type': 'HowTo',
        name: title,
        inLanguage: locale,
        description,
        step: host.steps.map((command, i) => ({
          '@type': 'HowToStep',
          position: i + 1,
          name: command,
          text: command,
        })),
      }
    : null;

  const articleLd = {
    '@context': 'https://schema.org',
    '@type': 'TechArticle',
    headline: title,
    description,
    inLanguage: locale,
    url: canonical,
    dateModified: BUILD_DATE,
    about: {
      '@type': 'SoftwareApplication',
      name: host ? host.name : 'Egent Code Plexus',
      applicationCategory: 'DeveloperApplication',
      url: host ? host.url : seo.repoUrl,
    },
    isBasedOn: seo.repoUrl,
  };

  const matrix = `
            <div class="table-container">
                <table class="saas-table">
                    <thead><tr><th>Host</th>${Object.values(integrations.channelLabels)
                      .map((label) => `<th>${escapeHtml(label)}</th>`)
                      .join('')}</tr></thead>
                    <tbody>
                    ${integrations.hosts
                      .map(
                        (h) =>
                          `<tr><td><a href="${host ? '../' : ''}${h.slug}/">${escapeHtml(h.name)}</a></td>${Object.keys(
                            integrations.channelLabels,
                          )
                            .map(
                              (channel) =>
                                `<td${h.channels.includes(channel) ? ' class="compare-best"' : ''}>${
                                  h.channels.includes(channel) ? '✓' : '—'
                                }</td>`,
                            )
                            .join('')}</tr>`,
                      )
                      .join('\n                    ')}
                    </tbody>
                </table>
            </div>`;

  const block = (heading, body) =>
    body ? `\n        <h2 class="section-heading">${escapeHtml(heading)}</h2>\n${body}` : '';
  const commands = (lines) =>
    `        <ul class="compare-method">${lines
      .map((line) => `<li><code class="mono">${escapeHtml(line)}</code></li>`)
      .join('')}</ul>`;

  const body = host
    ? [
        block(t.stepsHeading, host.steps ? commands(host.steps) : ''),
        block(
          t.configHeading,
          host.config
            ? `        <pre class="code-block mono">${escapeHtml(host.config)}</pre>` +
                (host.configPaths ? commands(host.configPaths) : '')
            : '',
        ),
        block(t.verifyHeading, commands([host.verify])),
        block(t.caveatHeading, `        <p class="hero-answer compare-limits">${copy.caveat}</p>`),
      ].join('')
    : block(t.matrixHeading, matrix) +
      block(
        t.hostsHeading,
        commands(integrations.hosts.map((h) => h.name)).replace(
          /<li><code class="mono">([^<]+)<\/code><\/li>/g,
          (_, name) => {
            const h = integrations.hosts.find((x) => x.name === name);
            return `<li><a href="${h.slug}/">${escapeHtml(h.name)}</a></li>`;
          },
        ),
      );

  const chrome = {
    header: applyTranslations(templateElement('header'), {}),
    footer: templateElement('footer').replace(/\s*<p class="footer-links">[\s\S]*?<\/p>/, ''),
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
    <script type="application/ld+json">${scriptJson(articleLd)}</script>${
      howToLd ? `\n    <script type="application/ld+json">${scriptJson(howToLd)}</script>` : ''
    }
</head>
<body class="theme-dark">
${chrome.header}

    <section class="hero compare-hero">
        <div class="hero-bg-glow"></div>
        <div class="container hero-content">
            <h1 class="hero-title">${escapeHtml(host ? host.name : t.title)}</h1>
            <p class="hero-tagline mono">${escapeHtml(t.subtitle)}</p>
            <p class="hero-answer">${host ? copy.intro : t.intro}</p>
        </div>
    </section>

    <main class="container compare-main">${body}
        <p class="table-caption">${escapeHtml(t.sourceLabel.replace('{version}', version))}</p>
        <p class="compare-back"><a href="${host ? '../../' : '../'}">&larr; ${escapeHtml(seo.meta[locale].ogTitle)}</a></p>
    </main>

${chrome.footer}
    <script>window.__ECP_LOCALE__ = "${locale}"; window.__ECP_ROOT__ = "${up}"; window.__ECP_PAGE__ = "${leaf}";</script>
    <script src="${up}js/qa_data.js"></script>
    <script src="${up}js/app.js"></script>
</body>
</html>
`;
}

// ── crawler files ────────────────────────────────────────────────────────────

/** Home outranks the comparison hub, which outranks one rival's slice of it. */
function priority(code, suffix) {
  const depth = suffix.split('/').filter(Boolean).length;
  const base = [1.0, 0.9, 0.7][depth];
  return (code === seo.defaultLocale ? base : base - 0.1).toFixed(1);
}

function sitemapXml() {
  const today = BUILD_DATE;
  const pages = Object.entries(seo.localePaths).flatMap(([code, p]) => [
    { code, p, suffix: '' },
    { code, p, suffix: 'compare/' },
    ...RIVALS.map((tool) => ({ code, p, suffix: `compare/${tool.slug}/` })),
    { code, p, suffix: 'integrations/' },
    ...integrations.hosts.map((host) => ({ code, p, suffix: `integrations/${host.slug}/` })),
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
        <priority>${priority(code, suffix)}</priority>
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

/**
 * Written, served, and read by nobody — for now.
 *
 * A crawler fetches robots.txt from the host root and nowhere else, and this
 * site lives on a project path, so the file below sits at
 * `<host>/egent-code-plexus/robots.txt` while every crawler asks
 * `<host>/robots.txt`, which is a different repository and currently 404s. A
 * 404 there means "no rules", so nothing is blocked; what is lost is the
 * `Sitemap:` line and the AI-crawler allowlist, and those crawlers default to
 * allowed anyway. Sitemap discovery therefore rests on Search Console and on
 * the IndexNow submission, not on this file.
 *
 * It stays because it costs nothing and becomes load-bearing the moment the
 * site answers at a host root — a custom domain, or a robots.txt in the user
 * site repository that points here.
 */
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

One page per comparison, same measurements, two columns:

${RIVALS.map((tool) => `- ecp vs ${tool.name}: ${BASE}compare/${tool.slug}/`).join('\n')}

## Wiring it into an agent

${integrations.hosts.map((host) => `- ${host.name} (${host.channels.join(', ')}): ${BASE}integrations/${host.slug}/`).join('\n')}

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
  // Compared with line endings normalised: a Windows checkout with
  // core.autocrlf=true rewrites the committed files to CRLF while the
  // generator still produces LF, which would report a fully current tree as
  // entirely stale. `.gitattributes` pins LF as well; this is the belt.
  const lf = (t) => t.replace(/\r\n/g, '\n');
  const current = existsSync(target) ? readFileSync(target, 'utf8') : null;
  if (current === null || lf(current) !== lf(content)) {
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
  // The footer is the only link from a landing page into the sub-pages, and it
  // named the comparison hub alone. A page nothing links to is a page a crawler
  // reaches only through the sitemap, and a reader never.
  html = html.replace(
    /<p class="footer-links">([\s\S]*?)<\/p>/,
    (_, links) =>
      `<p class="footer-links">${links} · <a href="integrations/">${escapeHtml(
        integrations.i18n[locale].title,
      )}</a></p>`,
  );
  emit(`${path}index.html`, html);
  emit(`${path}compare/index.html`, comparePage(locale, version));
  for (const rival of RIVALS) {
    emit(`${path}compare/${rival.slug}/index.html`, comparePage(locale, version, rival));
  }
  emit(`${path}integrations/index.html`, integrationPage(locale, version));
  for (const host of integrations.hosts) {
    emit(`${path}integrations/${host.slug}/index.html`, integrationPage(locale, version, host));
  }
}

// IndexNow verification. The key lives at the site's own path rather than the
// host root, because the host root is a different repository — which is why
// every submission has to carry `keyLocation` and can only vouch for URLs
// under this prefix.
if (seo.indexNowKey) emit(`${seo.indexNowKey}.txt`, seo.indexNowKey);
// Assets the pages reference but the generator does not produce. Not compared
// byte-for-byte — they are binaries built from scripts/site/og.svg — but their
// absence has to fail the check, since every page's og:image points at one and
// a missing file is a 404 on every share.
for (const asset of ['og.png', 'favicon.svg', 'favicon.ico', 'apple-touch-icon.png']) {
  const path = join(SITE, asset);
  if (!existsSync(path) || readFileSync(path).length === 0) {
    stale.push(`${asset} (missing or empty)`);
  }
  written.push(asset);
}

emit('sitemap.xml', sitemapXml());
emit('robots.txt', robotsTxt());
emit('llms.txt', llmsTxt(version, qas[seo.defaultLocale]));
emit('llms-full.txt', llmsFull(version, qas));
for (const [locale, path] of Object.entries(seo.localePaths)) {
  if (!path) continue;
  emit(`${path}llms.txt`, llmsTxt(version, qas[locale] ?? qas[seo.defaultLocale]));
}

// The sitemap is assembled from its own list of paths, so it can agree with
// itself while disagreeing with the pages that were actually written — a page
// nobody submits, or a submitted URL that 404s. Neither is visible in the
// output, so compare the two sets on every build rather than only in --check.
{
  const listed = new Set(
    [...readFileSync(join(SITE, 'sitemap.xml'), 'utf8').matchAll(/<loc>([^<]+)<\/loc>/g)]
      .map((m) => `${m[1].slice(BASE.length)}index.html`),
  );
  const emitted = new Set(written.filter((p) => p.endsWith('index.html')));
  const missing = [...emitted].filter((p) => !listed.has(p));
  const phantom = [...listed].filter((p) => !emitted.has(p));
  if (missing.length || phantom.length) {
    console.error(
      `sitemap disagrees with the generated pages:${
        missing.map((p) => `\n  built but not listed: ${p}`).join('')
      }${phantom.map((p) => `\n  listed but not built: ${p}`).join('')}`,
    );
    process.exit(1);
  }
}

if (CHECK && stale.length) {
  console.error(`site is stale, re-run scripts/site/build_site.mjs:\n  ${stale.join('\n  ')}`);
  process.exit(1);
}
console.log(`built ${written.length} files for v${version}: ${written.join(', ')}`);
