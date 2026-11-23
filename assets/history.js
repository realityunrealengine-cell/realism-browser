'use strict';
const main = document.getElementById('main');
const q = el('input', { type: 'search', placeholder: 'Поиск по истории' });
const list = el('div');
let items = [], timer = 0;

main.append(el('h1', { text: 'История' }), el('p', { class: 'sub', text: 'Приватные окна в историю не записываются.' }),
  el('div', { class: 'toolbar' }, q,
    el('button', { class: 'btn danger', text: 'Очистить всю историю', onclick: async () => { if (confirm('Удалить всю историю посещений?')) { await api('history/clear', {}); load(); } } })),
  el('div', { class: 'card' }, list));

q.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(load, 180); });

function render() {
  list.replaceChildren();
  if (!items.length) { list.append(el('div', { class: 'empty', text: q.value ? 'Ничего не найдено' : 'История пуста' })); return; }
  let day = '';
  for (const it of items) {
    const d = fmtDay(it.last);
    if (d !== day) { day = d; list.append(el('div', { class: 'day', text: d })); }
    list.append(el('div', { class: 'item' },
      el('div', { class: 'time', text: fmtTime(it.last) }), avatar(it.url),
      el('div', { class: 'grow', style: 'min-width:0;flex:1;cursor:pointer', onclick: (e) => openUrl(it.url, e.ctrlKey) },
        el('div', { class: 't', text: it.title || it.url }), el('div', { class: 'u', text: it.url + (it.visits > 1 ? `  ·  ${it.visits} посещ.` : '') })),
      el('div', { class: 'acts' },
        el('button', { class: 'btn small', text: 'В новой вкладке', onclick: () => openUrl(it.url, true) }),
        el('button', { class: 'btn small danger', text: 'Удалить', onclick: async () => { await api('history/delete', { ids: [it.id] }); load(); } }))));
  }
}
async function load() { items = await api('history?q=' + encodeURIComponent(q.value)); render(); }
load();
