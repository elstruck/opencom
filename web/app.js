// opencom web UI. Plain JS, no build step.
'use strict';

const qs = new URLSearchParams(location.search);
const KEY = qs.get('key') || localStorage.getItem('opencom.key') || '';
if (qs.get('key')) localStorage.setItem('opencom.key', KEY);

const S = {
  cfg: null,
  entries: new Map(),      // id -> entry
  order: [],               // ids in time order
  selected: localStorage.getItem('opencom.sel') || 'all',
  devices: [],
  font: +localStorage.getItem('opencom.font') || 16,
  autoScroll: true,
};

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const uuid = () => crypto.randomUUID ? crypto.randomUUID() : 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, c => { const r = Math.random() * 16 | 0; return (c === 'x' ? r : (r & 3 | 8)).toString(16); });
const PALETTE = ['#e4572e', '#17bebb', '#ffc914', '#76b041', '#a06cd5', '#3fa7ff', '#ff7eb6', '#f08a4b'];

async function api(path, opts = {}) {
  const headers = { 'content-type': 'application/json' };
  if (KEY) headers.authorization = 'Bearer ' + KEY;
  const r = await fetch('/api/v1' + path, { ...opts, headers: { ...headers, ...(opts.headers || {}) }, body: opts.body && typeof opts.body !== 'string' ? JSON.stringify(opts.body) : opts.body });
  if (!r.ok) {
    let msg = r.status + ' ' + r.statusText;
    try { msg = (await r.json()).error.message; } catch (_) {}
    throw new Error(msg);
  }
  return r.status === 204 ? null : r.headers.get('content-type')?.includes('json') ? r.json() : r.text();
}

function toast(msg, bad) {
  logStatus({ component: 'ui', ok: !bad, message: msg });
  if (bad) alert(msg);
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------
function showTab(name) {
  $$('.tabs button').forEach(b => b.classList.toggle('active', b.dataset.tab === name));
  $$('.tab').forEach(t => t.classList.toggle('hidden', t.id !== 'tab-' + name));
  location.hash = name === 'live' ? '' : name;
  if (!S.cfg) return;
  if (name === 'channels') { loadDevices(); renderChannelEditors(); }
  if (name === 'keywords') renderKeywords();
  if (name === 'automations') renderAutomations();
  if (name === 'osc') renderOsc();
  if (name === 'models') renderModels();
  if (name === 'settings') renderSettings();
}
$$('.tabs button').forEach(b => b.onclick = () => showTab(b.dataset.tab));

// ---------------------------------------------------------------------------
// Live view
// ---------------------------------------------------------------------------
const channelById = id => S.cfg?.channels.find(c => c.id === id);
const colorOf = ch => ch?.color || PALETTE[(S.cfg?.channels.indexOf(ch) ?? 0) % PALETTE.length];

function keywordsFor(chId) {
  const ch = channelById(chId);
  return [...(S.cfg?.keywords || []), ...(ch?.keywords || [])].filter(k => k.text?.trim());
}

function renderText(text, chId) {
  const kws = keywordsFor(chId);
  if (!kws.length) return esc(text);
  const isWord = c => /[\p{L}\p{N}']/u.test(c);
  const lower = text.toLowerCase();
  const spans = [];
  for (const k of kws) {
    const n = k.text.trim().toLowerCase();
    let i = 0;
    while ((i = lower.indexOf(n, i)) !== -1) {
      const e = i + n.length;
      const ok = k.wholeWord === false || ((i === 0 || !isWord(lower[i - 1])) && (e >= lower.length || !isWord(lower[e])));
      if (ok) spans.push([i, e, k]);
      i += 1;
    }
  }
  spans.sort((a, b) => a[0] - b[0] || (b[1] - b[0]) - (a[1] - a[0]));
  let out = '', last = 0;
  for (const [s, e, k] of spans) {
    if (s < last) continue;
    out += esc(text.slice(last, s));
    let shown = text.slice(s, e);
    if (k.sensitive) shown = shown.replace(/\S/g, '*');
    else if (k.replacement) shown = k.replacement;
    out += k.highlight === false ? esc(shown) : `<mark style="--hl:${esc(k.highlightColor || '#ffd400')}">${esc(shown)}</mark>`;
    last = e;
  }
  return out + esc(text.slice(last));
}

function visible(e) {
  if (S.selected === 'all') return true;
  if (S.selected.startsWith('g:')) {
    const g = S.cfg.groups.find(g => g.id === S.selected.slice(2));
    return g ? g.channelIds.includes(e.channelId) : true;
  }
  return e.channelId === S.selected;
}

function msgHtml(e) {
  const ch = channelById(e.channelId);
  const t = new Date(e.startedAt);
  const time = t.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const cls = ['msg', e.inProgress ? 'progress' : '', e.source !== 'audio' ? e.source : '', e.seen ? '' : 'unseen'].join(' ');
  return `<div class="${cls}" id="m-${e.id}" style="--c:${esc(colorOf(ch))}"><div class="meta"><span class="ch">${esc(ch?.name || '?')}</span>${time}</div><div class="text">${renderText(e.text, e.channelId)}</div></div>`;
}

function renderFeed() {
  const feed = $('#feed');
  const items = S.order.map(id => S.entries.get(id)).filter(e => e && visible(e));
  feed.innerHTML = items.length ? items.map(msgHtml).join('') : `<div class="empty">No messages yet.<br>Speak into a channel, or type below.</div>`;
  scrollFeed(true);
}

function upsertEntry(e, isNew) {
  const had = S.entries.has(e.id);
  S.entries.set(e.id, e);
  if (!had) S.order.push(e.id);
  const limit = 3000;
  while (S.order.length > limit) S.entries.delete(S.order.shift());
  if (!visible(e)) { renderChannelList(); return; }
  const feed = $('#feed');
  const el = $('#m-' + CSS.escape(e.id));
  const atBottom = nearBottom();
  if (el) el.outerHTML = msgHtml(e);
  else {
    $('.empty', feed)?.remove();
    feed.insertAdjacentHTML('beforeend', msgHtml(e));
  }
  if (atBottom || S.autoScroll) scrollFeed(); else $('#jumpLatest').classList.remove('hidden');
  if (!e.inProgress) renderChannelList();
}

function removeEntry(id) {
  S.entries.delete(id);
  S.order = S.order.filter(x => x !== id);
  $('#m-' + CSS.escape(id))?.remove();
}

const nearBottom = () => { const f = $('#feed'); return f.scrollHeight - f.scrollTop - f.clientHeight < 80; };
function scrollFeed(force) {
  const f = $('#feed');
  if (force || S.autoScroll) { f.scrollTop = f.scrollHeight; $('#jumpLatest').classList.add('hidden'); }
}
$('#feed').addEventListener('scroll', () => { S.autoScroll = nearBottom(); if (S.autoScroll) $('#jumpLatest').classList.add('hidden'); });
$('#jumpLatest').onclick = () => { S.autoScroll = true; scrollFeed(true); };

function unread(chId) {
  let n = 0;
  for (const e of S.entries.values()) if (e.channelId === chId && !e.seen && !e.inProgress) n++;
  return n;
}

function renderChannelList() {
  if (!S.cfg) return;
  const li = (key, name, color, count, cls = '') =>
    `<li data-key="${esc(key)}" class="${cls} ${S.selected === key ? 'selected' : ''}"><span class="dot" style="background:${esc(color)}"></span><span class="name">${esc(name)}</span><span class="badge ${count ? '' : 'zero'}">${count}</span><span class="meter"><i id="lv-${esc(key)}"></i></span></li>`;
  let total = 0;
  const chs = S.cfg.channels.map(ch => { const n = unread(ch.id); total += n; return li(ch.id, ch.name, colorOf(ch), n); });
  const groups = S.cfg.groups.map(g => li('g:' + g.id, g.name + ' (group)', g.color || '#888', g.channelIds.reduce((a, id) => a + unread(id), 0)));
  $('#channelList').innerHTML = li('all', 'All channels', '', total, 'all') + groups.join('') + chs.join('');
  const sel = $('#sendChannel');
  const cur = sel.value;
  sel.innerHTML = S.cfg.channels.filter(c => c.typingEnabled !== false).map(c => `<option value="${c.id}">${esc(c.name)}</option>`).join('');
  if (channelById(S.selected)) sel.value = S.selected; else if (cur) sel.value = cur;
}

$('#channelList').onclick = ev => {
  const li = ev.target.closest('li');
  if (!li) return;
  select(li.dataset.key);
};

function select(key) {
  S.selected = key;
  localStorage.setItem('opencom.sel', key);
  markRead(key);
  renderChannelList();
  renderFeed();
}

function markRead(key) {
  const ids = key === 'all' ? null : key.startsWith('g:') ? S.cfg.groups.find(g => 'g:' + g.id === key)?.channelIds : [key];
  for (const e of S.entries.values()) if (!ids || ids.includes(e.channelId)) e.seen = true;
  if (!ids) api('/transcript/read', { method: 'POST' }).catch(() => {});
  else ids.forEach(id => api('/transcript/read', { method: 'POST', body: { channelId: id } }).catch(() => {}));
}

$('#markAllRead').onclick = () => { markRead('all'); renderChannelList(); renderFeed(); };
$('#clearAll').onclick = async () => {
  const ch = channelById(S.selected);
  if (!confirm(ch ? `Clear the transcript of ${ch.name}?` : 'Clear the whole transcript?')) return;
  await api('/transcript/clear', { method: 'POST', body: ch ? { channelId: ch.id } : {} });
};

$('#sendForm').onsubmit = async ev => {
  ev.preventDefault();
  const text = $('#sendText').value.trim();
  const ch = $('#sendChannel').value;
  if (!text || !ch) return;
  try { await api(`/channels/${ch}/send`, { method: 'POST', body: { text } }); $('#sendText').value = ''; }
  catch (e) { toast(e.message, true); }
};

function setFont(px) {
  S.font = Math.max(11, Math.min(48, px));
  document.documentElement.style.setProperty('--font', S.font + 'px');
  localStorage.setItem('opencom.font', S.font);
}
$('#fontUp').onclick = () => setFont(S.font + 2);
$('#fontDown').onclick = () => setFont(S.font - 2);

let alertTimer;
function showAlert(text, seconds, color) {
  const a = $('#alert');
  a.textContent = text;
  a.style.setProperty('--ac', (color || '#d00000') + 'aa');
  a.classList.remove('hidden');
  clearTimeout(alertTimer);
  alertTimer = setTimeout(() => a.classList.add('hidden'), (seconds || 3) * 1000);
}

function uiCommand(cmd, chId) {
  const list = ['all', ...S.cfg.channels.map(c => c.id)];
  const i = list.indexOf(S.selected);
  switch (cmd) {
    case 'selectChannel': if (chId) select(chId); break;
    case 'nextChannel': select(list[(i + 1) % list.length]); break;
    case 'previousChannel': select(list[(i - 1 + list.length) % list.length]); break;
    case 'scrollToLatest': S.autoScroll = true; scrollFeed(true); break;
    case 'scrollUp': $('#feed').scrollBy(0, -300); break;
    case 'scrollDown': $('#feed').scrollBy(0, 300); break;
    case 'fontBigger': setFont(S.font + 2); break;
    case 'fontSmaller': setFont(S.font - 2); break;
    case 'markRead': markRead(chId || 'all'); renderFeed(); renderChannelList(); break;
    case 'showWindow': window.focus(); break;
  }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------
let ws, wsRetry = 0;
function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/api/v1/events${KEY ? '?key=' + encodeURIComponent(KEY) : ''}`);
  ws.onopen = async () => { $('#conn').classList.add('on'); wsRetry = 0; await reload(); };
  ws.onclose = () => { $('#conn').classList.remove('on'); setTimeout(connect, Math.min(5000, 500 * ++wsRetry)); };
  ws.onmessage = m => onEvent(JSON.parse(m.data));
}

let cfgReload;
function onEvent(ev) {
  switch (ev.event) {
    case 'entryAdded': upsertEntry(ev.entry, true); break;
    case 'entryUpdated': upsertEntry(ev.entry, false); break;
    case 'entryRemoved': removeEntry(ev.id); break;
    case 'transcriptCleared':
      for (const [id, e] of S.entries) if (!ev.channelId || e.channelId === ev.channelId) S.entries.delete(id);
      S.order = S.order.filter(id => S.entries.has(id));
      renderFeed(); renderChannelList(); break;
    case 'configChanged':
      clearTimeout(cfgReload);
      cfgReload = setTimeout(async () => { S.cfg = await api('/config'); renderAll(); }, 150);
      break;
    case 'alert': showAlert(ev.text, ev.seconds, ev.color); break;
    case 'ui': uiCommand(ev.command, ev.channelId); break;
    case 'level': {
      const el = document.getElementById('lv-' + ev.channelId);
      if (el) { el.style.width = Math.min(100, Math.sqrt(ev.rms) * 220) + '%'; el.classList.toggle('speaking', ev.speaking); }
      break;
    }
    case 'oscReceived': logOsc(ev); break;
    case 'status': logStatus(ev); if (ev.component === 'download' && /installed/.test(ev.message)) renderModels(); else if (ev.component === 'download') updateDownload(ev.message); break;
    case 'automationFired': logStatus({ component: 'automation', ok: true, message: `${ev.name} (${ev.cause})` }); break;
  }
}

function logStatus(ev) {
  const el = $('#statusLog');
  const line = document.createElement('div');
  line.className = ev.ok ? '' : 'bad';
  line.textContent = `${new Date().toLocaleTimeString()}  ${ev.component}: ${ev.message}`;
  el.prepend(line);
  while (el.childNodes.length > 200) el.lastChild.remove();
}
function logOsc(ev) {
  const el = $('#oscLog');
  const line = document.createElement('div');
  line.textContent = `${new Date().toLocaleTimeString()}  ${ev.address} ${ev.args.join(' ')}   ← ${ev.from}`;
  el.prepend(line);
  while (el.childNodes.length > 100) el.lastChild.remove();
}

async function reload() {
  S.cfg = await api('/config');
  const t = await api('/transcript?limit=2000');
  S.entries.clear(); S.order = [];
  for (const e of t) { S.entries.set(e.id, e); S.order.push(e.id); }
  renderAll();
  const st = await api('/status');
  for (const [k, v] of Object.entries(st.errors || {})) logStatus({ component: k, ok: false, message: v });
}

function renderAll() {
  $('#showName').textContent = S.cfg.name || '';
  document.title = (S.cfg.name ? S.cfg.name + ' · ' : '') + 'opencom';
  if (S.selected !== 'all' && !S.selected.startsWith('g:') && !channelById(S.selected)) S.selected = 'all';
  renderChannelList();
  renderFeed();
  const tab = $('.tabs button.active')?.dataset.tab;
  if (tab && tab !== 'live' && !document.activeElement?.closest('.page')) showTab(tab);
}

// ---------------------------------------------------------------------------
// Channels page
// ---------------------------------------------------------------------------
async function loadDevices() {
  try { S.devices = await api('/audio/devices'); } catch (_) { S.devices = []; }
  $('#deviceSummary').textContent = S.devices.length ? S.devices.map(d => `${d.name} (${d.channels} ch)`).join(', ') : 'none found';
  if (!document.activeElement?.closest('#channelEditors')) renderChannelEditors();
}

function sourceFields(ch) {
  const s = ch.source || { kind: 'none' };
  const devOpts = ['<option value="">Default input</option>', ...S.devices.map(d => `<option value="${esc(d.id)}" ${s.device === d.id || s.device === d.name ? 'selected' : ''}>${esc(d.name)} (${d.channels} ch)</option>`)];
  if (s.kind === 'device' && s.device && !S.devices.some(d => d.id === s.device || d.name === s.device)) devOpts.push(`<option value="${esc(s.device)}" selected>${esc(s.device)} (not connected)</option>`);
  const dev = S.devices.find(d => d.id === s.device || d.name === s.device) || S.devices.find(d => d.isDefault);
  const nch = Math.max(dev?.channels || 2, (s.channelIndex || 0) + 1);
  const chOpts = Array.from({ length: nch }, (_, i) => `<option value="${i}" ${s.channelIndex === i ? 'selected' : ''}>Input ${i + 1}</option>`).join('');
  return `
    <select data-f="source.kind">
      <option value="none" ${s.kind === 'none' ? 'selected' : ''}>No audio (typed only)</option>
      <option value="device" ${s.kind === 'device' ? 'selected' : ''}>Sound card input</option>
      <option value="file" ${s.kind === 'file' ? 'selected' : ''}>WAV file (test)</option>
    </select>
    ${s.kind === 'device' ? `<select data-f="source.device">${devOpts.join('')}</select><select data-f="source.channelIndex">${chOpts}</select>` : ''}
    ${s.kind === 'file' ? `<input class="grow" data-f="source.path" placeholder="/path/to/file.wav" value="${esc(s.path || '')}"><label><input type="checkbox" data-f="source.looped" ${s.looped ? 'checked' : ''}> loop</label>` : ''}`;
}

function renderChannelEditors() {
  if (!S.cfg) return;
  $('#channelEditors').innerHTML = S.cfg.channels.map(ch => `
    <div class="card" data-id="${ch.id}">
      <div class="row">
        <input type="color" data-f="color" value="${esc(colorOf(ch))}">
        <input class="grow" data-f="name" value="${esc(ch.name)}" placeholder="Channel name">
        <label><input type="checkbox" data-f="enabled" ${ch.enabled !== false ? 'checked' : ''}> transcribe</label>
        <label><input type="checkbox" data-f="typingEnabled" ${ch.typingEnabled !== false ? 'checked' : ''}> typing</label>
        <button class="danger" data-act="del">Delete</button>
      </div>
      <div class="row">${sourceFields(ch)}</div>
    </div>`).join('') || '<p class="hint">No channels yet.</p>';
}

$('#channelEditors').addEventListener('change', async ev => {
  const card = ev.target.closest('.card');
  const f = ev.target.dataset.f;
  if (!card || !f) return;
  const ch = structuredClone(channelById(card.dataset.id));
  let v = ev.target.type === 'checkbox' ? ev.target.checked : ev.target.value;
  if (f === 'source.channelIndex') v = +v;
  if (f.startsWith('source.')) {
    const k = f.slice(7);
    if (k === 'kind') ch.source = v === 'device' ? { kind: 'device', device: null, channelIndex: 0 } : v === 'file' ? { kind: 'file', path: '', looped: true } : { kind: 'none' };
    else ch.source[k] = k === 'device' && v === '' ? null : v;
  } else ch[f] = v;
  try { await api('/channels/' + ch.id, { method: 'PUT', body: ch }); }
  catch (e) { toast(e.message, true); }
  if (f === 'source.kind' || f === 'source.device') setTimeout(renderChannelEditors, 300);
});
$('#channelEditors').addEventListener('click', async ev => {
  if (ev.target.dataset.act !== 'del') return;
  const id = ev.target.closest('.card').dataset.id;
  if (confirm(`Delete channel ${channelById(id)?.name}?`)) await api('/channels/' + id, { method: 'DELETE' });
});
$('#addChannel').onclick = async () => {
  const n = S.cfg.channels.length;
  const used = S.cfg.channels.filter(c => c.source?.kind === 'device').map(c => c.source.channelIndex);
  let idx = 0; while (used.includes(idx)) idx++;
  await api('/channels', { method: 'POST', body: { name: 'Channel ' + (n + 1), color: PALETTE[n % PALETTE.length], source: { kind: 'device', device: null, channelIndex: idx } } });
  setTimeout(() => showTab('channels'), 200);
};

// ---------------------------------------------------------------------------
// Keywords page
// ---------------------------------------------------------------------------
function renderKeywords() {
  $('#keywordTable tbody').innerHTML = S.cfg.keywords.map(k => `
    <tr data-id="${k.id}">
      <td><input data-f="text" value="${esc(k.text)}"></td>
      <td><input type="checkbox" data-f="wholeWord" ${k.wholeWord !== false ? 'checked' : ''}></td>
      <td><input type="color" data-f="highlightColor" value="${esc(k.highlightColor || '#ffd400')}"></td>
      <td><input data-f="replacement" value="${esc(k.replacement || '')}" placeholder="(same)"></td>
      <td><input type="checkbox" data-f="sensitive" ${k.sensitive ? 'checked' : ''}></td>
      <td><button class="danger" data-act="del">✕</button></td>
    </tr>`).join('');
}
$('#keywordTable').addEventListener('change', async ev => {
  const tr = ev.target.closest('tr'), f = ev.target.dataset.f;
  if (!tr || !f) return;
  let v = ev.target.type === 'checkbox' ? ev.target.checked : ev.target.value;
  if (f === 'replacement' && !v) v = null;
  await api('/keywords/' + tr.dataset.id, { method: 'PUT', body: { [f]: v } }).catch(e => toast(e.message, true));
});
$('#keywordTable').addEventListener('click', async ev => {
  if (ev.target.dataset.act === 'del') await api('/keywords/' + ev.target.closest('tr').dataset.id, { method: 'DELETE' });
});
$('#addKeyword').onclick = async () => {
  const text = prompt('Keyword or phrase to highlight:');
  if (text?.trim()) await api('/keywords', { method: 'POST', body: { text: text.trim() } });
};

// ---------------------------------------------------------------------------
// Automations page
// ---------------------------------------------------------------------------
const ACTION_TEMPLATES = {
  osc: () => ({ type: 'osc', host: '127.0.0.1', port: 8000, address: '/cue/go', args: [] }),
  midi: () => ({ type: 'midi', port: null, message: { kind: 'noteOn', channel: 0, note: 60, velocity: 127 } }),
  alert: () => ({ type: 'alert', text: '{phrase}', seconds: 3, color: null }),
  sendText: () => ({ type: 'sendText', channelId: S.cfg.channels[0]?.id, text: 'Heard "{phrase}" on {channel}' }),
  http: () => ({ type: 'http', method: 'POST', url: 'http://127.0.0.1:8888/hook', headers: [], body: '{"text":"{text}"}' }),
  delay: () => ({ type: 'delay', seconds: 1 }),
  ui: () => ({ type: 'ui', command: 'scrollToLatest', channelId: null }),
};

function channelSelect(field, value, allowNone) {
  return `<select data-f="${field}">${allowNone ? '<option value="">(any)</option>' : ''}${S.cfg.channels.map(c => `<option value="${c.id}" ${c.id === value ? 'selected' : ''}>${esc(c.name)}</option>`).join('')}</select>`;
}

function actionFields(a, i) {
  const p = `actions.${i}.`;
  const destOpts = `<option value="">custom host</option>` + S.cfg.oscDestinations.map(d => `<option value="${d.id}" ${a.destinationId === d.id ? 'selected' : ''}>${esc(d.name)}</option>`).join('');
  switch (a.type) {
    case 'osc': return `<select data-f="${p}destinationId">${destOpts}</select>
      ${a.destinationId ? '' : `<input data-f="${p}host" value="${esc(a.host || '')}" placeholder="host" size="12"><input data-f="${p}port" type="number" value="${a.port || ''}" placeholder="port" style="width:80px">`}
      <input class="grow" data-f="${p}address" value="${esc(a.address)}" placeholder="/address">
      <input class="grow" data-f="${p}argsText" value="${esc((a.args || []).map(x => x.type + ':' + x.value).join(', '))}" placeholder="args, e.g. int:1, string:{text}" title="Comma-separated type:value pairs. Types: int, float, string, bool">`;
    case 'midi': {
      const m = a.message;
      return `<input data-f="${p}port" value="${esc(a.port || '')}" placeholder="port (first)" size="12">
        <select data-f="${p}message.kind">${['noteOn', 'noteOff', 'controlChange', 'programChange'].map(k => `<option ${m.kind === k ? 'selected' : ''}>${k}</option>`).join('')}</select>
        <label>ch <input type="number" min="1" max="16" data-f="${p}message.channel1" value="${(m.channel ?? 0) + 1}" style="width:56px"></label>
        ${m.kind === 'programChange' ? `<label>program <input type="number" min="0" max="127" data-f="${p}message.program" value="${m.program ?? 0}" style="width:64px"></label>`
          : m.kind === 'controlChange' ? `<label>cc <input type="number" min="0" max="127" data-f="${p}message.controller" value="${m.controller ?? 0}" style="width:64px"></label><label>value <input type="number" min="0" max="127" data-f="${p}message.value" value="${m.value ?? 0}" style="width:64px"></label>`
          : `<label>note <input type="number" min="0" max="127" data-f="${p}message.note" value="${m.note ?? 60}" style="width:64px"></label><label>vel <input type="number" min="0" max="127" data-f="${p}message.velocity" value="${m.velocity ?? 127}" style="width:64px"></label>`}`;
    }
    case 'alert': return `<input class="grow" data-f="${p}text" value="${esc(a.text)}"><label>sec <input type="number" step="0.5" data-f="${p}seconds" value="${a.seconds}" style="width:64px"></label><input type="color" data-f="${p}color" value="${esc(a.color || '#d00000')}">`;
    case 'sendText': return `${channelSelect(p + 'channelId', a.channelId)}<input class="grow" data-f="${p}text" value="${esc(a.text)}">`;
    case 'http': return `<select data-f="${p}method">${['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].map(m => `<option ${a.method.toUpperCase() === m ? 'selected' : ''}>${m}</option>`).join('')}</select><input class="grow" data-f="${p}url" value="${esc(a.url)}"><input class="grow" data-f="${p}body" value="${esc(a.body || '')}" placeholder="body">`;
    case 'delay': return `<label>wait <input type="number" step="0.1" min="0" data-f="${p}seconds" value="${a.seconds}" style="width:80px"> s</label>`;
    case 'ui': return `<select data-f="${p}command">${['scrollToLatest', 'selectChannel', 'nextChannel', 'previousChannel', 'markRead', 'clearTranscript', 'fontBigger', 'fontSmaller'].map(c => `<option ${a.command === c ? 'selected' : ''}>${c}</option>`).join('')}</select>${['selectChannel', 'markRead', 'clearTranscript'].includes(a.command) ? channelSelect(p + 'channelId', a.channelId, true) : ''}`;
  }
  return '';
}

function triggerFields(t) {
  const kinds = { phrase: 'When someone says', osc: 'When OSC arrives', webhook: 'When a webhook is called', manual: 'Only when run by hand' };
  const sel = `<select data-f="trigger.type">${Object.entries(kinds).map(([k, v]) => `<option value="${k}" ${t.type === k ? 'selected' : ''}>${v}</option>`).join('')}</select>`;
  if (t.type === 'phrase') return `${sel}<input class="grow" data-f="trigger.phrasesText" value="${esc((t.phrases || []).join(', '))}" placeholder="phrases, comma-separated">
    <label>on ${channelSelect('trigger.channel', (t.channelIds || [])[0], true)}</label>
    <label title="Fire on partial text, before the speaker finishes"><input type="checkbox" data-f="trigger.early" ${t.early ? 'checked' : ''}> early</label>
    <label><input type="checkbox" data-f="trigger.wholeWord" ${t.wholeWord !== false ? 'checked' : ''}> whole words</label>`;
  if (t.type === 'osc') return `${sel}<input class="grow" data-f="trigger.address" value="${esc(t.address || '')}" placeholder="/eos/out/event/cue/*/*/fire">`;
  if (t.type === 'webhook') return `${sel}<span class="hint">POST /api/v1/webhooks/</span><input data-f="trigger.path" value="${esc(t.path || '')}" placeholder="path">`;
  return sel;
}

function renderAutomations() {
  $('#automationEditors').innerHTML = S.cfg.automations.map(a => `
    <div class="card" data-id="${a.id}">
      <div class="row">
        <label><input type="checkbox" data-f="enabled" ${a.enabled !== false ? 'checked' : ''}></label>
        <input class="grow" data-f="name" value="${esc(a.name)}">
        <select data-f="policy" title="What happens when it triggers again while running">
          <option value="cancelAndRestart" ${a.policy === 'cancelAndRestart' ? 'selected' : ''}>restart if running</option>
          <option value="ignoreIfRunning" ${a.policy === 'ignoreIfRunning' ? 'selected' : ''}>ignore if running</option>
          <option value="runParallel" ${a.policy === 'runParallel' ? 'selected' : ''}>run in parallel</option>
        </select>
        <label>cooldown <input type="number" min="0" step="0.5" data-f="cooldownSeconds" value="${a.cooldownSeconds || 0}" style="width:64px"> s</label>
        <button data-act="run">Run now</button>
        <button class="danger" data-act="del">Delete</button>
      </div>
      <div class="row">${triggerFields(a.trigger)}</div>
      ${a.actions.map((x, i) => `<div class="row sub"><b>${i + 1}.</b> <span class="tag">${x.type}</span> ${actionFields(x, i)} <button data-act="up" data-i="${i}" title="Move up">↑</button><button class="danger" data-act="rm" data-i="${i}">✕</button></div>`).join('')}
      <div class="row sub"><select data-act="add"><option value="">+ add action…</option>${Object.keys(ACTION_TEMPLATES).map(k => `<option value="${k}">${k}</option>`).join('')}</select></div>
    </div>`).join('') || '<p class="hint">No automations yet.</p>';
}

function setPath(obj, path, v) {
  const parts = path.split('.');
  let o = obj;
  for (let i = 0; i < parts.length - 1; i++) o = o[isNaN(parts[i]) ? parts[i] : +parts[i]];
  o[parts.at(-1)] = v;
}

async function saveAutomation(a) {
  try { await api('/automations/' + a.id, { method: 'PUT', body: a }); }
  catch (e) { toast(e.message, true); }
}

$('#automationEditors').addEventListener('change', async ev => {
  const card = ev.target.closest('.card');
  if (!card) return;
  const a = structuredClone(S.cfg.automations.find(x => x.id === card.dataset.id));
  if (ev.target.dataset.act === 'add') {
    if (ev.target.value) a.actions.push(ACTION_TEMPLATES[ev.target.value]());
    return saveAutomation(a);
  }
  const f = ev.target.dataset.f;
  if (!f) return;
  let v = ev.target.type === 'checkbox' ? ev.target.checked : ev.target.value;
  if (ev.target.type === 'number') v = v === '' ? null : +v;
  if (f === 'trigger.type') {
    a.trigger = v === 'phrase' ? { type: 'phrase', phrases: [], channelIds: [], early: false, wholeWord: true } : v === 'osc' ? { type: 'osc', address: '/' } : v === 'webhook' ? { type: 'webhook', path: 'cue' } : { type: 'manual' };
  } else if (f === 'trigger.phrasesText') a.trigger.phrases = v.split(',').map(s => s.trim()).filter(Boolean);
  else if (f === 'trigger.channel') a.trigger.channelIds = v ? [v] : [];
  else if (f.endsWith('.argsText')) {
    const i = +f.split('.')[1];
    a.actions[i].args = v.split(',').map(s => s.trim()).filter(Boolean).map(s => {
      const m = s.match(/^(int|float|string|bool):(.*)$/);
      return m ? { type: m[1], value: m[2] } : { type: 'string', value: s };
    });
  } else if (f.endsWith('.message.channel1')) setPath(a, f.replace('channel1', 'channel'), Math.max(0, Math.min(15, v - 1)));
  else if (f.endsWith('.message.kind')) {
    const i = +f.split('.')[1], ch = a.actions[i].message.channel || 0;
    a.actions[i].message = v === 'programChange' ? { kind: v, channel: ch, program: 0 } : v === 'controlChange' ? { kind: v, channel: ch, controller: 0, value: 127 } : { kind: v, channel: ch, note: 60, velocity: v === 'noteOff' ? 0 : 127 };
  } else {
    if ((f.endsWith('destinationId') || f.endsWith('channelId') || f.endsWith('.port') && ev.target.type !== 'number') && v === '') v = null;
    setPath(a, f, v);
  }
  saveAutomation(a);
});

$('#automationEditors').addEventListener('click', async ev => {
  const act = ev.target.dataset.act;
  const card = ev.target.closest('.card');
  if (!card || !act || ev.target.tagName === 'SELECT') return;
  const a = structuredClone(S.cfg.automations.find(x => x.id === card.dataset.id));
  const i = +ev.target.dataset.i;
  if (act === 'run') return api(`/automations/${a.id}/run`, { method: 'POST' }).catch(e => toast(e.message, true));
  if (act === 'del') { if (confirm(`Delete automation ${a.name}?`)) await api('/automations/' + a.id, { method: 'DELETE' }); return; }
  if (act === 'rm') a.actions.splice(i, 1);
  if (act === 'up' && i > 0) [a.actions[i - 1], a.actions[i]] = [a.actions[i], a.actions[i - 1]];
  saveAutomation(a);
});

$('#addAutomation').onclick = async () => {
  await api('/automations', { method: 'POST', body: {
    name: 'New automation', policy: 'cancelAndRestart',
    trigger: { type: 'phrase', phrases: ['go'], channelIds: [], early: false, wholeWord: true },
    actions: [ACTION_TEMPLATES.alert()],
  } });
};

// ---------------------------------------------------------------------------
// OSC / MIDI page
// ---------------------------------------------------------------------------
async function renderOsc() {
  $('#destTable tbody').innerHTML = S.cfg.oscDestinations.map(d => `
    <tr data-id="${d.id}"><td><input data-f="name" value="${esc(d.name)}"></td><td><input data-f="host" value="${esc(d.host)}"></td>
    <td><input data-f="port" type="number" value="${d.port}"></td><td><input type="checkbox" data-f="enabled" ${d.enabled !== false ? 'checked' : ''}></td>
    <td><button class="danger" data-act="del">✕</button></td></tr>`).join('');
  $('#oscTestDest').innerHTML = S.cfg.oscDestinations.map(d => `<option value="${d.id}">${esc(d.name)}</option>`).join('') || '<option value="">(add a destination first)</option>';
  $('#oscPort').textContent = S.cfg.settings.oscListenPort ?? 'off';
  try {
    const ports = await api('/midi/ports');
    $('#midiPorts').innerHTML = ports.filter(p => p.isOutput).map(p => `<li>${esc(p.name)}</li>`).join('') || '<li>No MIDI outputs found.</li>';
  } catch (_) {}
}
$('#destTable').addEventListener('change', async ev => {
  const tr = ev.target.closest('tr'), f = ev.target.dataset.f;
  if (!tr || !f) return;
  let v = ev.target.type === 'checkbox' ? ev.target.checked : ev.target.type === 'number' ? +ev.target.value : ev.target.value;
  await api('/osc/destinations/' + tr.dataset.id, { method: 'PUT', body: { [f]: v } }).catch(e => toast(e.message, true));
});
$('#destTable').addEventListener('click', async ev => {
  if (ev.target.dataset.act === 'del') await api('/osc/destinations/' + ev.target.closest('tr').dataset.id, { method: 'DELETE' });
});
$('#addDest').onclick = () => api('/osc/destinations', { method: 'POST', body: { name: 'Lighting console', host: '192.168.1.100', port: 8000 } });
$('#oscTest').onsubmit = async ev => {
  ev.preventDefault();
  const arg = $('#oscTestArg').value;
  try {
    await api('/osc/send', { method: 'POST', body: { destinationId: $('#oscTestDest').value || null, address: $('#oscTestAddr').value, args: arg ? [{ type: 'string', value: arg }] : [] } });
    logStatus({ component: 'osc', ok: true, message: 'sent ' + $('#oscTestAddr').value });
  } catch (e) { toast(e.message, true); }
};

// ---------------------------------------------------------------------------
// Models page
// ---------------------------------------------------------------------------
async function renderModels() {
  const m = await api('/models');
  $('#modelsDir').textContent = m.modelsDir;
  $('#modelList').innerHTML = m.models.map(x => `
    <div class="card ${x.active ? 'active' : ''}" data-id="${x.id}">
      <h4>${esc(x.name)}</h4>
      <div><span class="tag">${x.downloadMb} MB</span><span class="tag">${esc(x.languages.length > 20 ? x.languages.split(',').length + ' languages' : x.languages)}</span>${x.installed ? '<span class="tag">installed</span>' : ''}${x.active ? '<span class="tag" style="color:var(--accent)">active</span>' : ''}</div>
      <p>${esc(x.description)}</p>
      <div class="dl hint"></div>
      ${x.installed ? (x.active ? '' : `<button data-act="use">Use this model</button>`) : `<button data-act="dl">Download</button>`}
    </div>`).join('');
}
function updateDownload(msg) {
  for (const card of $$('#modelList .card')) {
    const name = $('h4', card).textContent;
    if (msg.startsWith(name)) $('.dl', card).textContent = msg.slice(name.length + 2);
  }
}
$('#modelList').addEventListener('click', async ev => {
  const card = ev.target.closest('.card');
  if (!card) return;
  if (ev.target.dataset.act === 'dl') { ev.target.disabled = true; $('.dl', card).textContent = 'starting…'; await api(`/models/${card.dataset.id}/download`, { method: 'POST' }).catch(e => toast(e.message, true)); }
  if (ev.target.dataset.act === 'use') { await api('/settings', { method: 'PUT', body: { stt: { model: card.dataset.id } } }); setTimeout(renderModels, 300); }
});

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------
async function renderSettings() {
  const f = $('#settingsForm'), s = S.cfg.settings;
  f.showName.value = S.cfg.name || '';
  f.threads.value = s.stt.threads; f.partialInterval.value = s.stt.partialInterval; f.vadThreshold.value = s.stt.vadThreshold;
  f.minSilence.value = s.stt.minSilence; f.maxSpeech.value = s.stt.maxSpeech; f.oscListenPort.value = s.oscListenPort ?? '';
  f.apiKey.value = s.apiKey || ''; f.historyLimit.value = s.historyLimit;
  const st = await api('/status');
  $('#savePath').textContent = st.savePath || '(not saved)';
}
$('#settingsForm').onsubmit = async ev => {
  ev.preventDefault();
  const f = ev.target;
  try {
    const cfg = structuredClone(S.cfg);
    cfg.name = f.showName.value;
    Object.assign(cfg.settings.stt, { threads: +f.threads.value, partialInterval: +f.partialInterval.value, vadThreshold: +f.vadThreshold.value, minSilence: +f.minSilence.value, maxSpeech: +f.maxSpeech.value });
    cfg.settings.oscListenPort = f.oscListenPort.value ? +f.oscListenPort.value : null;
    cfg.settings.apiKey = f.apiKey.value || null;
    cfg.settings.historyLimit = +f.historyLimit.value;
    await api('/config', { method: 'PUT', body: cfg });
    if (cfg.settings.apiKey) localStorage.setItem('opencom.key', cfg.settings.apiKey); else localStorage.removeItem('opencom.key');
    logStatus({ component: 'settings', ok: true, message: 'saved (OSC port changes need a restart)' });
  } catch (e) { toast(e.message, true); }
};
$('#loadShow').onchange = async ev => {
  const file = ev.target.files[0];
  if (!file) return;
  try { await api('/config', { method: 'PUT', body: await file.text() }); logStatus({ component: 'settings', ok: true, message: 'show loaded: ' + file.name }); }
  catch (e) { toast('Could not load show: ' + e.message, true); }
};

// ---------------------------------------------------------------------------
// Keyboard shortcuts (when not typing)
// ---------------------------------------------------------------------------
document.addEventListener('keydown', ev => {
  if (ev.target.closest('input, select, textarea')) return;
  if (ev.key === '/') { ev.preventDefault(); $('#sendText').focus(); }
  else if (ev.key === 'ArrowDown' && ev.altKey) uiCommand('nextChannel');
  else if (ev.key === 'ArrowUp' && ev.altKey) uiCommand('previousChannel');
  else if (ev.key === 'End') uiCommand('scrollToLatest');
  else if (ev.key === '+' || ev.key === '=') setFont(S.font + 2);
  else if (ev.key === '-') setFont(S.font - 2);
});

setFont(S.font);
connect();
showTab(location.hash.slice(1) || 'live');
