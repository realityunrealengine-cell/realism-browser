'use strict';
const main = document.getElementById('main');
const list = el('div');
const dirHint = el('p', { class: 'sub' });
main.append(el('h1', { text: 'Загрузки' }), dirHint,
  el('div', { class: 'toolbar' }, el('button', { class: 'btn', text: 'Очистить список', onclick: async () => { await api('downloads/clear', {}); load(); } }),
    el('a', { class: 'btn', href: '/settings', text: 'Папка загрузок…' })),
  list);

const LABEL = { running: 'Загружается', paused: 'Пауза', done: 'Готово', failed: 'Ошибка', canceled: 'Отменено' };
const act = (id, action) => api('downloads/action', { id, action }).then(load);

function render(d) {
  dirHint.textContent = 'Файлы сохраняются в ' + d.dir;
  list.replaceChildren();
  if (!d.items.length) { list.append(el('div', { class: 'card empty', text: 'Загрузок пока нет' })); return; }
  for (const x of d.items) {
    const pct = x.total > 0 ? Math.min(100, x.received / x.total * 100) : (x.state === 'done' ? 100 : 0);
    const info = x.state === 'running'
      ? `${fmtBytes(x.received)} из ${x.total > 0 ? fmtBytes(x.total) : '?'} · ${fmtBytes(x.speed)}/с` + (x.speed > 0 && x.total > 0 ? ` · осталось ~${fmtDur(Math.round((x.total - x.received) / x.speed))}` : '')
      : x.state === 'done' ? fmtBytes(x.total > 0 ? x.total : x.received) + (x.exists ? '' : ' · файл удалён') : LABEL[x.state];
    const acts = el('div', { style: 'display:flex;gap:6px' });
    if (x.state === 'done' && x.exists) acts.append(el('button', { class: 'btn small primary', text: 'Открыть', onclick: () => act(x.id, 'open') }), el('button', { class: 'btn small', text: 'Показать в папке', onclick: () => act(x.id, 'show') }));
    if (x.state === 'running') acts.append(el('button', { class: 'btn small', text: 'Пауза', onclick: () => act(x.id, 'pause') }), el('button', { class: 'btn small danger', text: 'Отмена', onclick: () => act(x.id, 'cancel') }));
    if (x.state === 'paused') acts.append(el('button', { class: 'btn small primary', text: 'Продолжить', onclick: () => act(x.id, 'resume') }), el('button', { class: 'btn small danger', text: 'Отмена', onclick: () => act(x.id, 'cancel') }));
    if (!['running', 'paused'].includes(x.state)) acts.append(el('button', { class: 'btn small', text: 'Убрать', onclick: () => act(x.id, 'remove') }));
    list.append(el('div', { class: 'card', style: 'padding:14px 18px' },
      el('div', { style: 'display:flex;align-items:center;gap:12px' }, avatar(x.url),
        el('div', { style: 'flex:1;min-width:0' }, el('div', { class: 'ellipsis', style: 'font-weight:600;max-width:none', text: x.name }),
          el('div', { class: 'faint ellipsis', style: 'font-size:12px;max-width:none', text: x.url })),
        el('span', { class: 'pill ' + ({ done: 'ok', running: 'acc', failed: 'bad' }[x.state] || ''), text: LABEL[x.state] })),
      ['running', 'paused'].includes(x.state) ? el('div', { class: 'progress', style: 'margin:12px 0 8px' }, el('i', { style: `width:${pct.toFixed(1)}%` })) : null,
      el('div', { style: 'display:flex;align-items:center;gap:10px;margin-top:8px' }, el('div', { class: 'muted', style: 'flex:1;font-size:12.5px', text: info }), acts)));
  }
}
let busy = false;
async function load() { const d = await api('downloads'); busy = d.items.some(x => x.state === 'running'); render(d); }
load();
setInterval(() => { if (busy && !document.hidden) load(); }, 700);
setInterval(() => { if (!busy && !document.hidden) load(); }, 4000);
