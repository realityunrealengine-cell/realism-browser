"""Builds bench/results.md from the full run (all browsers) and the Realism re-run
(scenarios affected by the TrySuspend fix). Realism rows of re-run scenarios replace the old ones."""
import csv, statistics, sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent / "bench"
full = list(csv.DictReader(open(root / "raw-full-run.csv", encoding="utf-8")))
rerun_path = root / "realism-rerun" / "raw.csv"
rerun = list(csv.DictReader(open(rerun_path, encoding="utf-8"))) if rerun_path.exists() else []
rerun_keys = {(r["scenario"], r["browser"]) for r in rerun}
rows = [r for r in full if (r["scenario"], r["browser"]) not in rerun_keys] + rerun

SC = [("idle", "Запуск без вкладок (стартовая страница)"), ("blank", "Одна вкладка about:blank"), ("google", "Одна вкладка Google"),
      ("wiki5", "5 одинаковых сайтов (Wikipedia)"), ("wiki10", "10 одинаковых сайтов (Wikipedia), замер через ~25 с"),
      ("wiki10idle", "10 вкладок после 5 минут бездействия"), ("youtube", "Воспроизведение видео YouTube (1080p+, без звука)")]
BR = [("realism", "Realism — Сбалансированный (по умолчанию)"), ("realism-saver", "Realism — Экономия памяти"),
      ("chrome", "Google Chrome"), ("edge", "Microsoft Edge")]

def med(vals):
    vals = [v for v in vals if v is not None]
    return statistics.median(vals) if vals else None

def agg(sc, br):
    rs = [r for r in rows if r["scenario"] == sc and r["browser"] == br and float(r["private_ws_mb"]) > 0]
    if not rs:
        return None
    f = lambda k: [float(r[k]) for r in rs if r[k] != ""]
    ram = f("private_ws_mb")
    return dict(n=len(rs), ram=med(ram), rmin=min(ram), rmax=max(ram), commit=med(f("commit_mb")),
                cpu=med(f("cpu_pct")), start=med(f("startup_ms")), procs=med(f("processes")),
                rerun=(sc, br) in rerun_keys)

def pct(a, b):
    if a is None or b is None or b == 0:
        return "—"
    d = (a - b) / b * 100
    return f"{'+' if d > 0 else ''}{d:.0f}%"

out = ["# Realism Browser — сравнение потребления ресурсов", "",
       "Intel Core i5-13400F (16 потоков), 16 ГБ RAM, Windows 11. Chrome и Edge — установленные версии, Brave на машине не установлен.",
       "Каждый запуск: новый временный профиль, окно 1280×800, одинаковые URL и ожидание, звук выключен у всех (`--mute-audio`), перед замерами — прогрев каждого браузера.",
       "Память — сумма по **всему дереву процессов** (браузер, рендереры, GPU, утилиты, crashpad). Медиана по запускам; в скобках — min–max.", "",
       "* **RAM** — private working set (колонка «Память» в Диспетчере задач): сколько физической памяти занято только этим браузером.",
       "* **Commit** — выделенная приватная память (с учётом выгруженного в файл подкачки).",
       "* **CPU** — средняя загрузка всех ядер за окно замера; **Старт** — время до появления окна.", ""]
for sc, label in SC:
    out.append(f"## {label}\n")
    out.append("| Браузер | RAM, МБ | Commit, МБ | CPU, % | Старт, мс | Процессов | RAM vs Chrome | RAM vs Edge | Commit vs Chrome | Commit vs Edge |")
    out.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    ch, ed = agg(sc, "chrome"), agg(sc, "edge")
    for bk, bl in BR:
        a = agg(sc, bk)
        if not a:
            continue
        mark = " ¹" if a["rerun"] else ""
        isref = bk in ("chrome", "edge")
        cols = ["—"] * 4 if isref else [pct(a["ram"], ch and ch["ram"]), pct(a["ram"], ed and ed["ram"]),
                                        pct(a["commit"], ch and ch["commit"]), pct(a["commit"], ed and ed["commit"])]
        out.append(f"| {bl}{mark} | **{a['ram']:.0f}** ({a['rmin']:.0f}–{a['rmax']:.0f}) | {a['commit']:.0f} | {a['cpu']:.2f} | {a['start']:.0f} | {a['procs']:.0f} | " + " | ".join(cols) + " |")
    out.append("")
out += ["¹ Перемерено после исправления заморозки фоновых вкладок (TrySuspend не срабатывал для вкладок, ни разу не бывших видимыми); "
        "строки Chrome/Edge — из того же полного прогона на той же машине.", "",
        "Отрицательный процент — Realism расходует меньше. Сырые данные: `raw-full-run.csv`, `realism-rerun/raw.csv`."]
(root / "results.md").write_text("\n".join(out), encoding="utf-8")
print("\n".join(out))
