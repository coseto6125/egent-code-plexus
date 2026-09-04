// One form per MCP-derived tool schema, posted to /api/run against the repo
// the visitor indexed. `API` lets the same page run from a static host
// against a remote backend.
const API = window.ECP_DEMO_API || '';
const POLL_MS = 2000;

const $ = (id) => document.getElementById(id);
const state = { meta: null, repos: [], repo: null, tool: null, poll: null };

async function boot() {
  const res = await fetch(`${API}/api/meta`);
  if (!res.ok) {
    setStatus('add-status', `backend unavailable (${res.status})`, true);
    return;
  }
  state.meta = await res.json();
  $('version').textContent = `v${state.meta.version}`;
  const l = state.meta.limits;
  $('limits').textContent = `Repositories up to ${l.max_repo_mb} MB · ${l.max_repos} kept at a time · queries stop after ${l.timeout_secs} s`;
  renderTools();
  await refreshRepos();
  const hash = location.hash.slice(1);
  const [repo, tool] = hash.includes('@') ? hash.split('@') : [hash, ''];
  if (repo && !state.repos.some((r) => r.name === repo)) await addRepo(repo);
  selectRepo(state.repos.find((r) => r.name === repo)?.name ?? state.repos.find((r) => r.status === 'ready')?.name ?? null);
  selectTool(state.meta.tools.find((t) => t.subcommand === tool)?.subcommand ?? 'find');
}

async function refreshRepos() {
  const res = await fetch(`${API}/api/repos`);
  if (!res.ok) return;
  state.repos = (await res.json()).repos;
  renderRepos();
  const busy = state.repos.some((r) => r.status !== 'ready' && r.status !== 'failed');
  if (busy && !state.poll) state.poll = setInterval(refreshRepos, POLL_MS);
  if (!busy && state.poll) {
    clearInterval(state.poll);
    state.poll = null;
  }
  if (state.repo === null) {
    const ready = state.repos.find((r) => r.status === 'ready');
    if (ready) selectRepo(ready.name);
  }
}

async function addRepo(url) {
  setStatus('add-status', 'queuing…');
  $('add-btn').disabled = true;
  try {
    const res = await fetch(`${API}/api/repos`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ url }),
    });
    const data = await res.json();
    if (data.error) {
      setStatus('add-status', data.error, true);
      return;
    }
    setStatus('add-status', data.repo.status === 'ready' ? `${data.repo.name} is ready` : `${data.repo.name}: ${data.repo.status}…`);
    $('add-url').value = '';
    await refreshRepos();
    selectRepo(data.repo.name);
  } catch (e) {
    setStatus('add-status', `request failed: ${e.message}`, true);
  } finally {
    $('add-btn').disabled = false;
  }
}

function renderRepos() {
  const box = $('repos');
  $('repos-empty').hidden = state.repos.length > 0;
  box.replaceChildren(
    ...state.repos.map((r) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = `item ${r.status}`;
      b.dataset.name = r.name;
      b.classList.toggle('active', r.name === state.repo);
      const m = r.summary?.summary?.per_repo?.[0]?.metrics ?? null;
      const stats = m ? Object.entries(m).filter(([, v]) => typeof v === 'number').slice(0, 3).map(([k, v]) => `${v} ${k}`).join(' · ') : '';
      const sub = r.status === 'ready' ? [r.commit, stats].filter(Boolean).join(' · ') : r.status === 'failed' ? r.error : `${r.status}…`;
      b.innerHTML = `<span class="name">${esc(r.name)}</span><span class="sub">${esc(sub)}</span>`;
      b.onclick = () => selectRepo(r.name);
      return b;
    }),
  );
  if (state.repo) {
    const current = state.repos.find((r) => r.name === state.repo);
    if (current?.status === 'ready') setStatus('add-status', '');
  }
}

function renderTools() {
  const box = $('tools');
  box.replaceChildren(
    ...state.meta.tools.map((t) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'item';
      b.dataset.name = t.subcommand;
      b.innerHTML = `<span class="name">ecp ${esc(t.subcommand)}</span><span class="sub">${esc(firstSentence(t.description))}</span>`;
      b.onclick = () => selectTool(t.subcommand);
      return b;
    }),
  );
}

function selectRepo(name) {
  state.repo = name;
  for (const el of $('repos').children) el.classList.toggle('active', el.dataset.name === name);
  syncHash();
}

function selectTool(sub) {
  state.tool = state.meta.tools.find((t) => t.subcommand === sub);
  for (const el of $('tools').children) el.classList.toggle('active', el.dataset.name === sub);
  $('tool-name').textContent = `ecp ${sub}`;
  $('tool-desc').textContent = state.tool.description;
  renderFields(state.tool);
  clearOutput();
  syncHash();
}

function syncHash() {
  if (state.tool) history.replaceState(null, '', `#${state.repo ?? ''}@${state.tool.subcommand}`);
}

function renderFields(tool) {
  const props = tool.schema.properties ?? {};
  const required = new Set(tool.schema.required ?? []);
  const order = [...tool.positional_args.filter((p) => p in props), ...Object.keys(props).filter((k) => !tool.positional_args.includes(k))];
  $('fields').replaceChildren(...order.map((key) => fieldFor(key, props[key], required.has(key))));
}

function fieldFor(key, prop, required) {
  const wrap = document.createElement('div');
  wrap.className = 'field';
  const id = `f-${key}`;
  const label = document.createElement('label');
  label.htmlFor = id;
  label.innerHTML = `${esc(key)}${required ? '<span class="req">required</span>' : ''}`;
  let input;
  if (prop.type === 'boolean') {
    wrap.classList.add('bool');
    input = document.createElement('input');
    input.type = 'checkbox';
  } else if (Array.isArray(prop.enum)) {
    input = document.createElement('select');
    input.append(new Option('', ''));
    for (const v of prop.enum) input.append(new Option(v, v));
  } else if (prop.type === 'integer' || prop.type === 'number') {
    input = document.createElement('input');
    input.type = 'number';
  } else if (key === 'query' || key === 'pattern') {
    input = document.createElement('textarea');
    wrap.classList.add('wide');
  } else {
    input = document.createElement('input');
    input.type = 'text';
  }
  input.id = id;
  input.name = key;
  if (required) input.required = true;
  const help = document.createElement('div');
  help.className = 'help';
  help.textContent = prop.description ?? '';
  help.title = prop.description ?? '';
  if (prop.type === 'boolean') wrap.append(input, label);
  else wrap.append(label, input);
  wrap.append(help);
  return wrap;
}

function collectArgs() {
  const args = {};
  for (const el of $('form').elements) {
    if (!el.name) continue;
    if (el.type === 'checkbox') {
      if (el.checked) args[el.name] = true;
    } else if (el.value !== '') {
      args[el.name] = el.type === 'number' ? Number(el.value) : el.value;
    }
  }
  return args;
}

async function run(ev) {
  ev.preventDefault();
  if (!state.repo) {
    setStatus('status', 'index a repository first', true);
    return;
  }
  const body = { tool: state.tool.subcommand, repo: state.repo, args: collectArgs() };
  $('run').disabled = true;
  setStatus('status', 'running…');
  const started = performance.now();
  try {
    const res = await fetch(`${API}/api/run`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    const data = await res.json();
    if (data.error) {
      setStatus('status', data.error, true);
      return;
    }
    showOutcome(data, performance.now() - started);
  } catch (e) {
    setStatus('status', `request failed: ${e.message}`, true);
  } finally {
    $('run').disabled = false;
  }
}

let lastOutcome = null;

function showOutcome(o, roundTrip) {
  lastOutcome = o;
  $('cmd').textContent = o.argv.map(shellQuote).join(' ');
  $('copy').hidden = false;
  const parts = [`exit ${o.exit_code ?? 'killed'}`, `${o.elapsed_ms} ms in ecp`, `${Math.round(roundTrip)} ms round trip`];
  if (o.truncated) parts.push('output truncated');
  if (o.timed_out) parts.push('timed out');
  $('out-meta').textContent = parts.join(' · ');
  setStatus('status', o.timed_out ? 'timed out' : '', o.timed_out || (o.exit_code ?? 1) !== 0);
  renderOutput();
  $('err').textContent = o.stderr;
  $('err-wrap').hidden = !o.stderr.trim();
  $('out-wrap').hidden = false;
}

function renderOutput() {
  if (!lastOutcome) return;
  const text = lastOutcome.stdout;
  if ($('raw').checked) {
    $('out').textContent = text;
    return;
  }
  try {
    $('out').innerHTML = highlight(JSON.stringify(JSON.parse(text), null, 2));
  } catch {
    $('out').textContent = text;
  }
}

function clearOutput() {
  lastOutcome = null;
  $('cmd').textContent = '';
  $('copy').hidden = true;
  $('out-wrap').hidden = true;
  setStatus('status', '');
}

function setStatus(id, text, bad = false) {
  $(id).textContent = text;
  $(id).classList.toggle('bad', bad);
}

function highlight(json) {
  return esc(json).replace(
    /("(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*"(?:\s*:)?|\b(?:true|false|null)\b|-?\d+(?:\.\d*)?(?:[eE][+-]?\d+)?)/g,
    (m) => {
      if (m.startsWith('"')) return m.endsWith(':') ? `<span class="k">${m}</span>` : `<span class="s">${m}</span>`;
      if (m === 'true' || m === 'false' || m === 'null') return `<span class="b">${m}</span>`;
      return `<span class="n">${m}</span>`;
    },
  );
}

function shellQuote(s) {
  return /^[\w@%+=:,./-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}

function firstSentence(s) {
  const cut = s.search(/[.:—] /);
  return cut > 0 ? s.slice(0, cut) : s;
}

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

$('add-form').addEventListener('submit', (ev) => {
  ev.preventDefault();
  addRepo($('add-url').value);
});
$('form').addEventListener('submit', run);
$('raw').addEventListener('change', renderOutput);
$('copy').addEventListener('click', () => navigator.clipboard?.writeText($('cmd').textContent));
boot();
