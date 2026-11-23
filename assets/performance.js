'use strict';
const main = document.getElementById('main');
const STATE = { active: ['Активна', 'ok'], background: ['В фоне', 'acc'], suspended: ['Спит (заморожена)', 'warn'], discarded: ['Выгружена', 'warn'], ntp: ['Новая вкладка (без движка)', ''] };
let last = null, before = null;

function render(d) {
  main.replaceChildren();
  main.append(el('h1', { text: 'Монитор ресурсов' }),
    el('p', { class: 'sub', text: 'Реальные данные Windows по всему дереву процессов браузера (интерфейс Realism + все процессы WebView2). Обновление каждые 2 секунды.' }));
  const live = d.counts.live, sleep = d.counts.suspended + d.counts.discarded;
  main.append(el('div', { class: 'grid stats' },
    el('div', { class: 'stat hero' }, el('div', { class: 'k', text: 'Память браузера целиком (интерфейс + WebView2)' }), el('div', { class: 'v', text: fmtMB(d.total.private_ws) })),
    el('div', { class: 'stat' }, el('div', { class: 'k', text: 'Выделено (commit)' }), el('div', { class: 'v', text: fmtMB(d.total.commit) })),
    el('div', { class: 'stat' }, el('div', { class: 'k', text: 'CPU (все ядра)' }), el('div', { class: 'v' }, d.cpu.toFixed(1), el('small', { text: ' %' }))),
    el('div', { class: 'stat' }, el('div', { class: 'k', text: 'Процессов' }), el('div', { class: 'v', text: d.proc_count })),
    el('div', { class: 'stat' }, el('div', { class: 'k', text: 'Активных вкладок' }), el('div', { class: 'v', text: live })),
    el('div', { class: 'stat' }, el('div', { class: 'k', text: 'Спящих вкладок' }), el('div', { class: 'v', text: sleep }, el('small', { text: d.counts.discarded ? `  (${d.counts.discarded} выгружено)` : '' })))));

  // Breakdown that matches Windows Task Manager line by line.
  const ui = d.processes.filter(p => p.name.toLowerCase() === 'realism.exe');
  const eng = d.processes.filter(p => p.name.toLowerCase() !== 'realism.exe');
  const sum = (a, k) => a.reduce((s, p) => s + p[k], 0);
  main.append(el('div', { class: 'card' },
    el('h2', { text: 'Как сверить с Диспетчером задач' }),
    el('div', { class: 'row' }, el('div', { class: 'grow' },
      el('div', { class: 'label', text: 'Интерфейс Realism — realism.exe' }),
      el('div', { class: 'hint', text: 'В Диспетчере задач — строка «Realism Browser» (PID ' + ui.map(p => p.pid).join(', ') + ').' })),
      el('div', { class: 'v', style: 'font-weight:650;font-variant-numeric:tabular-nums', text: fmtMB(sum(ui, 'private_ws')) })),
    el('div', { class: 'row' }, el('div', { class: 'grow' },
      el('div', { class: 'label', text: `Движок страниц WebView2 — ${eng.length} процесс(ов) msedgewebview2.exe` }),
      el('div', { class: 'hint', text: eng.length
        ? 'Диспетчер задач показывает их отдельными строками «Microsoft Edge WebView2», а не внутри «Realism Browser». Найти: вкладка «Подробности», PID ' + eng.map(p => p.pid).join(', ') + '. Другие программы Windows тоже используют WebView2 — их процессы сюда не входят.'
        : 'Движок сейчас остановлен: ни одна вкладка не загружена.' })),
      el('div', { class: 'v', style: 'font-weight:650;font-variant-numeric:tabular-nums', text: fmtMB(sum(eng, 'private_ws')) })),
    el('div', { class: 'row' }, el('div', { class: 'grow label', text: 'Итого браузер (сумма двух строк выше)' }),
      el('div', { class: 'v', style: 'font-weight:700', text: fmtMB(d.total.private_ws) })),
    el('p', { class: 'faint', style: 'font-size:12px;margin:8px 0 0', text: 'Метрика та же, что в колонке «Память» Диспетчера (private working set). Небольшая разница возможна: Диспетчер и эта страница обновляются в разные моменты.' })));

  const freeBtn = el('button', { class: 'btn primary', text: 'Освободить память', onclick: freeMem });
  const p = d.policy;
  main.append(el('div', { class: 'card' },
    el('div', { class: 'row' },
      el('div', { class: 'grow' }, el('div', { class: 'label' }, 'Режим: ', el('span', { class: 'pill acc', text: d.mode_label })),
        el('div', { class: 'hint', text: `Заморозка фоновых вкладок через ${fmtDur(p.suspend_after)} · выгрузка через ${fmtDur(p.discard_after)}` +
          (p.max_live_bg ? ` · не более ${p.max_live_bg} живых фоновых` : '') + (p.low_mem_on_hide ? ' · низкий приоритет памяти сразу при скрытии' : (p.low_mem_after ? ` · низкий приоритет памяти через ${fmtDur(p.low_mem_after)} в фоне` : '')) +
          ` · движок останавливается без вкладок через ${fmtDur(p.engine_idle)}` })),
      el('a', { href: '/settings', class: 'btn', text: 'Изменить' }), freeBtn),
    d.engine_restart_needed ? el('div', { class: 'notice', text: 'Изоляция процессов нового режима применится после перезапуска браузера.' }) : null,
    before ? el('div', { class: 'row' }, el('div', { class: 'grow hint', text: `После «Освободить память»: было ${fmtMB(before)}, сейчас ${fmtMB(d.total.private_ws)} (${before > d.total.private_ws ? '−' + fmtMB(before - d.total.private_ws) : 'без изменений'}).` })) : null,
    el('div', { class: 'row' }, el('div', { class: 'grow hint', text: `Движок WebView2: ${d.engine === 'running' ? 'запущен' : d.engine === 'starting' ? 'запускается' : 'остановлен (ни одной загруженной вкладки — память движка освобождена)'}` }))));

  // Tabs
  const tb = el('tbody');
  for (const t of d.tabs) {
    const [label, cls] = STATE[t.state] || [t.state, ''];
    const acts = el('td', { class: 'num' });
    if (t.state === 'background') acts.append(el('button', { class: 'btn small', text: 'Усыпить', onclick: () => tabAct(t.id, 'sleep') }));
    acts.append(' ', el('button', { class: 'btn small', text: 'Открыть', onclick: () => tabAct(t.id, 'activate') }));
    tb.append(el('tr', {},
      el('td', {}, el('div', { class: 'ellipsis', style: 'max-width:340px;font-weight:500', text: (t.private ? '🔒 ' : '') + t.title }), el('div', { class: 'ellipsis faint', style: 'max-width:340px;font-size:12px', text: t.url })),
      el('td', {}, el('span', { class: 'pill ' + cls, text: label }), t.audio ? el('span', { class: 'pill ok', style: 'margin-left:6px', text: '♪ звук' }) : null,
        t.protected && t.state === 'background' ? el('div', { class: 'faint', style: 'font-size:12px;margin-top:3px', text: 'Не выгружается: ' + t.protected }) : null),
      el('td', { class: 'num' }, t.mem !== null ? fmtMB(t.mem) : (t.state === 'discarded' || t.state === 'ntp' ? '0 МБ' : '—'),
        t.shared > 1 ? el('div', { class: 'faint', style: 'font-size:11px', text: `процесс на ${t.shared} вкл.` }) : null),
      acts));
  }
  main.append(el('div', { class: 'card' }, el('h2', { text: 'Вкладки' }),
    el('table', {}, el('thead', {}, el('tr', {}, el('th', { text: 'Страница' }), el('th', { text: 'Состояние' }), el('th', { class: 'num', text: 'Память*' }), el('th', {}))), tb),
    el('p', { class: 'faint', style: 'font-size:12px;margin:10px 0 0', text: d.per_tab_available
      ? '* Приблизительно: private working set процесса-рендерера, делённый между вкладками, которые он обслуживает. Общие процессы (браузер, GPU, сеть) в эту колонку не входят.'
      : '* Память по вкладкам доступна, когда запущен движок WebView2.' })));

  // Processes
  const pb = el('tbody');
  for (const pr of d.processes.sort((a, b) => b.private_ws - a.private_ws)) {
    pb.append(el('tr', {}, el('td', { text: pr.kind }), el('td', { class: 'faint', text: pr.name + ' · ' + pr.pid }),
      el('td', { class: 'num', text: fmtMB(pr.private_ws) }), el('td', { class: 'num faint', text: fmtMB(pr.commit) }), el('td', { class: 'num', text: pr.cpu.toFixed(1) + ' %' })));
  }
  main.append(el('div', { class: 'card' }, el('h2', { text: 'Процессы' }),
    el('table', {}, el('thead', {}, el('tr', {}, el('th', { text: 'Тип' }), el('th', { text: 'Процесс' }), el('th', { class: 'num', text: 'Private WS' }), el('th', { class: 'num', text: 'Commit' }), el('th', { class: 'num', text: 'CPU' }))), pb)));
}

async function refresh() {
  try { last = await api('perf'); render(last); } catch (e) { main.replaceChildren(el('p', { class: 'empty', text: 'Нет данных: ' + e.message })); }
}
async function freeMem() {
  before = last ? last.total.private_ws : null;
  const r = await api('perf/free', {});
  toast(r.actions ? `Усыплено/выгружено вкладок: ${r.actions}` : 'Нечего освобождать: все фоновые вкладки уже спят или защищены');
  setTimeout(refresh, 1500);
}
async function tabAct(id, action) { await api('tab', { id, action }); setTimeout(refresh, 600); }
refresh();
setInterval(() => { if (!document.hidden) refresh(); }, 2000);
