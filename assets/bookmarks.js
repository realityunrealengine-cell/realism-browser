'use strict';
const main = document.getElementById('main');
let all = [], cur = 1;
const tree = el('div', { class: 'tree' });
const pane = el('div');
main.append(el('h1', { text: 'Закладки' }), el('p', { class: 'sub', text: 'Закладки из «Панели закладок» показываются под адресной строкой (Ctrl+Shift+B).' }),
  el('div', { class: 'toolbar' },
    el('button', { class: 'btn primary', text: '+ Закладка', onclick: () => edit({ parent: cur, folder: false }) }),
    el('button', { class: 'btn', text: '+ Папка', onclick: () => edit({ parent: cur, folder: true }) })),
  el('div', { class: 'two' }, el('div', { class: 'card', style: 'padding:10px' }, tree), el('div', { class: 'card' }, pane)));

const kids = p => all.filter(b => b.parent === p).sort((a, b) => a.pos - b.pos || a.id - b.id);
const byId = id => all.find(b => b.id === id);

function folderPath(id) { const out = []; let b = byId(id); while (b) { out.unshift(b); b = byId(b.parent); } return out; }

function renderTree() {
  tree.replaceChildren();
  const walk = (p, depth) => {
    for (const f of kids(p).filter(b => b.folder)) {
      tree.append(el('a', { class: f.id === cur ? 'on' : '', style: `padding-left:${10 + depth * 14}px`, onclick: () => { cur = f.id; render(); } }, '📁 ' + f.title));
      walk(f.id, depth + 1);
    }
  };
  walk(0, 0);
}

function renderPane() {
  pane.replaceChildren(el('h2', { text: folderPath(cur).map(f => f.title).join(' › ') }));
  const items = kids(cur);
  if (!items.length) { pane.append(el('div', { class: 'empty', text: 'Папка пуста' })); return; }
  items.forEach((b, idx) => {
    pane.append(el('div', { class: 'item' },
      b.folder ? el('div', { class: 'fav', style: 'background:var(--warn)', text: '📁' }) : avatar(b.url),
      el('div', { style: 'flex:1;min-width:0;cursor:pointer', onclick: (e) => b.folder ? (cur = b.id, render()) : openUrl(b.url, e.ctrlKey) },
        el('div', { class: 't', text: b.title || b.url }), b.folder ? null : el('div', { class: 'u', text: b.url })),
      el('div', { class: 'acts' },
        el('button', { class: 'btn small', text: '↑', disabled: idx === 0, onclick: () => move(b, cur, idx - 1) }),
        el('button', { class: 'btn small', text: '↓', disabled: idx === items.length - 1, onclick: () => move(b, cur, idx + 1) }),
        el('button', { class: 'btn small', text: 'Изменить', onclick: () => edit(b) }),
        el('button', { class: 'btn small danger', text: 'Удалить', onclick: async () => { if (!b.folder || confirm('Удалить папку со всем содержимым?')) { await api('bookmarks/delete', { id: b.id }); load(); } } }))));
  });
}
function render() { renderTree(); renderPane(); }

async function move(b, parent, pos) { await api('bookmarks/move', { id: b.id, parent, pos }); load(); }

function edit(b) {
  const title = el('input', { type: 'text', value: b.title || '' });
  const url = el('input', { type: 'text', value: b.url || '', placeholder: 'https://' });
  const folderSel = el('select');
  for (const f of all.filter(x => x.folder && x.id !== b.id)) folderSel.append(el('option', { value: f.id, text: folderPath(f.id).map(x => x.title).join(' › '), selected: f.id === (b.parent || cur) }));
  const d = el('dialog', {},
    el('h2', { text: b.id ? (b.folder ? 'Папка' : 'Закладка') : (b.folder ? 'Новая папка' : 'Новая закладка') }),
    el('label', { text: 'Название' }), title,
    b.folder ? null : el('label', { text: 'Адрес' }), b.folder ? null : url,
    el('label', { text: 'Папка' }), folderSel,
    el('div', { class: 'dlg-acts' }, el('button', { class: 'btn', text: 'Отмена', onclick: () => d.close() }),
      el('button', { class: 'btn primary', text: 'Сохранить', onclick: async () => {
        const parent = Number(folderSel.value);
        if (b.id) {
          await api('bookmarks/update', { id: b.id, title: title.value, url: url.value });
          if (parent !== b.parent) await api('bookmarks/move', { id: b.id, parent, pos: 9999 });
        } else {
          if (!b.folder && !url.value.trim()) return;
          await api('bookmarks/add', { parent, title: title.value || url.value, url: url.value, folder: b.folder });
        }
        d.close(); load();
      } })));
  d.addEventListener('close', () => d.remove());
  document.body.append(d); d.showModal(); title.focus();
}
async function load() { all = await api('bookmarks'); if (!byId(cur)) cur = 1; render(); }
load();
