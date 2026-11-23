'use strict';
const main = document.getElementById('main');
const keys = [['Ctrl+T', 'Новая вкладка'], ['Ctrl+W', 'Закрыть вкладку'], ['Ctrl+Shift+T', 'Вернуть закрытую вкладку'], ['Ctrl+L / Alt+D / F6', 'Адресная строка'],
  ['Ctrl+R / F5', 'Обновить'], ['Ctrl+Shift+R / Ctrl+F5', 'Обновить без кэша'], ['Ctrl+Tab / Ctrl+Shift+Tab', 'Следующая / предыдущая вкладка'], ['Ctrl+1…8, Ctrl+9', 'Перейти к вкладке'],
  ['Ctrl+N / Ctrl+Shift+N', 'Новое окно / приватное окно'], ['Ctrl+H / Ctrl+J', 'История / загрузки'], ['Ctrl+D', 'Добавить в закладки'], ['Ctrl+Shift+B', 'Панель закладок'],
  ['Ctrl+ + / Ctrl+ − / Ctrl+0', 'Масштаб'], ['F11', 'Полноэкранный режим'], ['Alt+← / Alt+→', 'Назад / вперёд'], ['Shift+Esc', 'Монитор ресурсов'], ['Ctrl+F', 'Поиск на странице'], ['F12', 'Инструменты разработчика']];
const updBox = el('div', { class: 'card', id: 'upd' });
let updTimer = 0;
async function updAct(path) { renderUpd(await api(path, {})); }
function renderUpd(u) {
  updBox.replaceChildren(el('h2', { text: 'Обновления' }));
  const row = (label, hint, ...btns) => el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: label }), hint ? el('div', { class: 'hint', text: hint }) : null), ...btns);
  const r = u.release || {};
  const check = el('button', { class: 'btn', text: 'Проверить обновления', onclick: () => updAct('update/check') });
  const last = u.last_check ? 'Последняя проверка: ' + new Date(u.last_check).toLocaleString('ru-RU') : 'Ещё не проверялось';
  switch (u.state) {
    case 'checking': updBox.append(row('Проверяю наличие новой версии…', 'Источник: github.com/' + u.repo)); break;
    case 'available':
      updBox.append(row('Доступна версия ' + r.tag, `У вас ${u.current}. Размер ${fmtBytes(r.size)}.`,
        el('button', { class: 'btn primary', text: 'Скачать обновление', onclick: () => updAct('update/download') })));
      if (r.notes) updBox.append(el('pre', { style: 'white-space:pre-wrap;font:13px/1.5 inherit;color:var(--dim);margin:6px 0 0', text: r.notes }));
      break;
    case 'downloading': {
      const pct = u.total ? Math.min(100, u.received / u.total * 100) : 0;
      updBox.append(row('Скачиваю ' + r.tag + '…', `${fmtBytes(u.received)} из ${fmtBytes(u.total)}`),
        el('div', { class: 'progress' }, el('i', { style: `width:${pct.toFixed(1)}%` })));
      break;
    }
    case 'ready':
      updBox.append(row('Обновление ' + r.tag + ' скачано и проверено (SHA-256)',
        u.downloads_running ? 'Внимание: идут загрузки файлов — они прервутся.' : 'Браузер закроется, обновится и откроется снова. Вкладки сохранятся.',
        el('button', { class: 'btn primary', text: 'Установить и перезапустить', onclick: () => updAct('update/install') })));
      break;
    case 'uptodate': updBox.append(row('Установлена последняя версия', `${u.current} · ${last}`, check)); break;
    case 'error': updBox.append(row('Ошибка обновления', u.error, check)); break;
    default: updBox.append(row('Текущая версия ' + u.current, last, check));
  }
  const auto = el('input', { type: 'checkbox' }); auto.checked = u.auto;
  auto.addEventListener('change', async () => renderUpd(await api('update/auto', { on: auto.checked })));
  updBox.append(el('div', { class: 'row' }, el('div', { class: 'grow' }, el('div', { class: 'label', text: 'Проверять автоматически раз в день' }),
    el('div', { class: 'hint' }, 'Релизы: ', el('a', { href: 'https://github.com/' + u.repo + '/releases', target: '_blank', text: 'github.com/' + u.repo }))),
    el('label', { class: 'switch' }, auto, el('span'))));
  clearTimeout(updTimer);
  if (u.state === 'checking' || u.state === 'downloading') updTimer = setTimeout(async () => renderUpd(await api('update')), 500);
}

(async function () {
  const a = await api('about');
  const kv = (k, v) => el('div', { class: 'row' }, el('div', { class: 'grow label', text: k }), el('div', { class: 'muted', style: 'text-align:right;word-break:break-all;max-width:60%', text: v }));
  main.append(el('h1', { text: 'Realism Browser' }), el('p', { class: 'sub', text: 'Нативный интерфейс на Rust (Win32 + Direct2D), страницы — Microsoft Edge WebView2.' }),
    updBox,
    el('div', { class: 'card' }, kv('Версия', a.version), kv('WebView2 Runtime', a.webview2 || 'не найден'), kv('Движок', a.engine === 'running' ? 'запущен' : a.engine === 'starting' ? 'запускается' : 'остановлен'),
      kv('Программа', a.exe), kv('Данные профиля', a.data_dir)),
    el('div', { class: 'card' }, el('h2', { text: 'Горячие клавиши' }), keys.map(([k, v]) => el('div', { class: 'row' }, el('div', { class: 'grow', text: v }), el('span', { class: 'kbd', text: k })))),
    el('div', { class: 'card' }, el('h2', { text: 'Ограничения движка WebView2' }), el('ul', { class: 'muted' },
      ['Расширения Chrome Web Store не поддерживаются.', 'Синхронизация с аккаунтом не поддерживается.', 'DRM-видео (Widevine) недоступно: WebView2 поддерживает только PlayReady — часть платных стримингов может не воспроизводиться.',
        'При выгрузке вкладки сохраняется история «Назад» (по URL), но не состояние формы и прокрутки.', 'Вкладки нельзя перетащить в другое окно.'].map(t => el('li', { text: t })))));
  renderUpd(await api('update'));
})();
