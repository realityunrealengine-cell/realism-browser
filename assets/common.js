'use strict';
const TOKEN = document.querySelector('meta[name="rb-token"]').content;

async function api(path, body) {
  const opt = { method: body === undefined ? 'GET' : 'POST', headers: { 'X-Realism-Token': TOKEN } };
  if (body !== undefined) { opt.headers['Content-Type'] = 'application/json'; opt.body = JSON.stringify(body); }
  const r = await fetch('/api/' + path, opt);
  if (!r.ok) throw new Error('HTTP ' + r.status);
  return r.json();
}

/** Safe DOM builder: never uses innerHTML for data. */
function el(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k === 'text') e.textContent = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (k === 'style') e.style.cssText = v;
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false) e.append(k instanceof Node ? k : document.createTextNode(String(k)));
  return e;
}

function fmtBytes(n) {
  if (n === null || n === undefined || n < 0) return '—';
  if (n < 1024) return n + ' Б';
  const u = ['КБ', 'МБ', 'ГБ', 'ТБ']; let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return (n >= 100 || i === 0 ? n.toFixed(0) : n.toFixed(1)) + ' ' + u[i];
}
function fmtMB(n) { return n === null || n === undefined ? '—' : (n / 1048576).toFixed(n < 10485760 ? 1 : 0) + ' МБ'; }
function fmtDur(s) {
  if (s === null || s === undefined) return 'никогда';
  if (s < 60) return s + ' с';
  if (s < 3600) return Math.round(s / 60) + ' мин';
  return (s / 3600).toFixed(s % 3600 ? 1 : 0) + ' ч';
}
function fmtTime(ms) { return new Date(ms).toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' }); }
function fmtDay(ms) {
  const d = new Date(ms), t = new Date(); const y = new Date(); y.setDate(t.getDate() - 1);
  if (d.toDateString() === t.toDateString()) return 'Сегодня';
  if (d.toDateString() === y.toDateString()) return 'Вчера';
  return d.toLocaleDateString('ru-RU', { weekday: 'long', day: 'numeric', month: 'long', year: d.getFullYear() === t.getFullYear() ? undefined : 'numeric' });
}
function hostOf(u) { try { return new URL(u).hostname.replace(/^www\./, ''); } catch { return u; } }
const PALETTE = ['#8B7CFF', '#3DD9C5', '#F2706B', '#F2B84B', '#5AA9F2', '#D37CF2', '#6BD46B', '#F28F4B'];
function avatar(url) {
  const h = hostOf(url || '?'); let x = 0x811c9dc5; for (const c of h) { x ^= c.charCodeAt(0); x = Math.imul(x, 16777619) >>> 0; }
  return el('div', { class: 'fav', style: 'background:' + PALETTE[x % PALETTE.length], text: (h[0] || '?').toUpperCase() });
}
function toast(msg) {
  let t = document.querySelector('.toast');
  if (!t) { t = el('div', { class: 'toast' }); document.body.append(t); }
  t.textContent = msg; t.classList.add('show');
  clearTimeout(t._h); t._h = setTimeout(() => t.classList.remove('show'), 2200);
}
function openUrl(url, newtab) { api('open', { url, newtab: !!newtab }); }

// Sidebar
(function () {
  const pages = [['settings', '', 'Настройки'], ['history', '', 'История'], ['bookmarks', '', 'Закладки'],
    ['downloads', '', 'Загрузки'], ['performance', '', 'Производительность'], ['about', '', 'О программе']];
  const cur = location.pathname.replace(/^\//, '').split('/')[0];
  const nav = document.querySelector('nav.side');
  if (!nav) return;
  nav.append(el('div', { class: 'brand' }, el('div', { class: 'logo', text: 'R' }), el('span', {}, el('b', { text: 'Realism' }), ' Browser')));
  for (const [p, ico, label] of pages) nav.append(el('a', { href: '/' + p, class: p === cur ? 'on' : '' }, el('span', { class: 'ico', text: ico }), label));
  nav.append(el('div', { class: 'foot', text: 'realism://' + cur }));
})();
