'use strict';
const main = document.getElementById('main');
let S = null;

function seg(options, value, onpick) {
  const s = el('div', { class: 'seg' });
  for (const [v, label] of options) s.append(el('button', { class: v === value ? 'on' : '', text: label, onclick: () => onpick(v) }));
  return s;
}
function sw(checked, onchange) {
  const i = el('input', { type: 'checkbox' }); i.checked = !!checked; i.addEventListener('change', () => onchange(i.checked));
  return el('label', { class: 'switch' }, i, el('span'));
}
async function set(key, value, msg) { await api('settings/set', { key, value }); if (msg) toast(msg); await load(); }

function render() {
  main.replaceChildren(el('h1', { text: 'Настройки' }), el('p', { class: 'sub', text: 'Изменения применяются сразу.' }));

  main.append(el('div', { class: 'card' }, el('h2', { text: 'Внешний вид' }),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Тема' }), el('div', { class: 'hint', text: 'Интерфейс браузера и встроенные страницы. Сайты получают prefers-color-scheme.' })),
      seg([['dark', 'Тёмная'], ['light', 'Светлая'], ['system', 'Как в Windows']], S.theme, v => set('theme', v).then(() => location.reload()))),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Панель закладок' })), sw(S.bookmarks_bar, v => set('bookmarks_bar', v)))));

  main.append(el('div', { class: 'card' }, el('h2', { text: 'Поиск и запуск' }),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Поисковая система' })),
      seg([['google', 'Google'], ['bing', 'Bing'], ['duckduckgo', 'DuckDuckGo']], S.search, v => set('search', v, 'Поисковая система изменена'))),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'При запуске' }), el('div', { class: 'hint', text: 'Восстановленные вкладки загружаются только при открытии — запуск остаётся быстрым.' })),
      seg([['restore', 'Восстановить вкладки'], ['newtab', 'Новая вкладка']], S.startup, v => set('startup', v)))));

  const modes = el('div', { class: 'grid modes' });
  for (const m of S.modes) {
    const li = [
      `заморозка фоновых через ${fmtDur(m.suspend_after)}`,
      `выгрузка через ${fmtDur(m.discard_after)}`,
      m.max_live_bg ? `не более ${m.max_live_bg} живых фоновых вкладок` : 'без лимита живых вкладок',
      m.low_mem_on_hide ? 'низкий приоритет памяти сразу при скрытии' : (m.low_mem_after ? `низкий приоритет памяти через ${fmtDur(m.low_mem_after)} в фоне` : 'низкий приоритет памяти только при заморозке'),
      m.process_per_site ? 'вкладки одного сайта делят процесс' : 'отдельный процесс на вкладку',
      m.no_spare_renderer ? 'без запасного процесса-рендерера' : 'запасной рендерер для быстрых переходов',
      m.engine_idle ? `движок выключается без вкладок через ${fmtDur(m.engine_idle)}` : 'движок всегда запущен (быстрее открытие)',
    ];
    modes.append(el('div', { class: 'mode' + (m.key === S.mode ? ' on' : ''), onclick: () => set('mode', m.key, 'Режим: ' + m.label) },
      el('b', { text: m.label }), el('ul', {}, li.map(x => el('li', { text: x })))));
  }
  const exc = el('textarea', { placeholder: 'например: music.yandex.ru\nmail.google.com' });
  exc.value = S.exceptions.join('\n');
  main.append(el('div', { class: 'card' }, el('h2', { text: 'Производительность и память' }), modes,
    S.engine_restart_needed ? el('div', { class: 'notice', text: 'Изоляция процессов нового режима применится после перезапуска браузера.' }) : null,
    el('div', { class: 'row', style: 'margin-top:12px' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Никогда не усыплять сайты' }),
      el('div', { class: 'hint', text: 'По одному домену в строке. Вкладки со звуком, камерой/микрофоном, загрузками и несохранённым вводом защищены автоматически.' }))),
    exc, el('div', { style: 'margin-top:8px;display:flex;gap:8px' },
      el('button', { class: 'btn', text: 'Сохранить исключения', onclick: () => set('exceptions', exc.value.split(/\s+/).filter(Boolean), 'Исключения сохранены') }),
      el('a', { class: 'btn', href: '/performance', text: 'Открыть монитор ресурсов' }))));

  main.append(el('div', { class: 'card' }, el('h2', { text: 'Загрузки' }),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Папка' }), el('div', { class: 'hint', text: S.download_dir })),
      el('button', { class: 'btn', text: 'Изменить…', onclick: async () => { await api('settings/pick-dir', {}); setTimeout(load, 800); } }),
      S.custom_download_dir ? el('button', { class: 'btn', text: 'По умолчанию', onclick: () => set('download_dir_reset', true) }) : null),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Спрашивать, куда сохранять файл' })), sw(S.ask_download, v => set('ask_download', v)))));

  const pt = el('tbody');
  for (const p of S.permissions) {
    pt.append(el('tr', {}, el('td', { text: hostOf(p.origin) }), el('td', { text: p.label }),
      el('td', {}, el('span', { class: 'pill ' + (p.state === 1 ? 'ok' : 'bad'), text: p.state === 1 ? 'разрешено' : 'заблокировано' })),
      el('td', { class: 'num' }, el('button', { class: 'btn small', text: 'Сбросить', onclick: async () => { await api('permissions/delete', { origin: p.origin, kind: p.kind }); load(); } }))));
  }
  const pops = S.popups.map(o => el('div', { class: 'row' }, el('div', { class: 'grow', text: o }),
    el('button', { class: 'btn small', text: 'Удалить', onclick: async () => { await api('popups/delete', { origin: o }); load(); } })));
  main.append(el('div', { class: 'card' }, el('h2', { text: 'Разрешения сайтов' }),
    S.permissions.length ? el('table', {}, el('thead', {}, el('tr', {}, el('th', { text: 'Сайт' }), el('th', { text: 'Доступ' }), el('th', { text: 'Решение' }), el('th', {}))), pt)
      : el('p', { class: 'faint', text: 'Сайты ещё не запрашивали камеру, микрофон, геолокацию или уведомления.' }),
    el('h2', { style: 'margin-top:18px', text: 'Всплывающие окна разрешены' }),
    pops.length ? pops : el('p', { class: 'faint', text: 'Всплывающие окна без действия пользователя блокируются на всех сайтах.' })));

  const chk = {};
  const opt = (k, label, on) => { const i = el('input', { type: 'checkbox' }); i.checked = on; chk[k] = i; return el('label', { style: 'display:flex;gap:8px;align-items:center;padding:4px 0' }, i, label); };
  main.append(el('div', { class: 'card' }, el('h2', { text: 'Очистка данных' }),
    opt('history', 'История посещений', true), opt('cookies', 'Cookies и данные сайтов (вы выйдете из аккаунтов)', false),
    opt('cache', 'Кэш изображений и файлов', true), opt('downloads', 'Список загрузок', false), opt('passwords', 'Сохранённые пароли и данные автозаполнения', false),
    el('div', { style: 'margin-top:10px' }, el('button', { class: 'btn danger', text: 'Очистить выбранное', onclick: async () => {
      const body = {}; for (const k in chk) body[k] = chk[k].checked;
      const r = await api('clear-data', body); toast(r.ok ? 'Данные очищены' : 'Не удалось очистить часть данных');
    } }))));

  main.append(el('div', { class: 'card' }, el('h2', { text: 'Профиль' }),
    el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Папка данных' }), el('div', { class: 'hint', text: S.data_dir })))));
}
async function load() { S = await api('settings'); render(); }
load();
