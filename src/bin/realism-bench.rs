//! realism-bench — reproducible RAM/CPU/startup comparison of Realism Browser with Chrome, Edge and Brave.
//!
//! Every run uses a fresh temporary profile, the same URLs, the same window size and waiting times.
//! Memory is measured over the *whole process tree* of each browser (browser, renderers, GPU, utility,
//! crashpad...), not just the main executable.
//!
//! Usage:
//!   realism-bench [--runs 3] [--idle-runs 2] [--idle-minutes 5] [--settle 20]
//!                 [--browsers realism,realism-saver,chrome,edge,brave] [--scenarios all|idle,blank,google,wiki5,wiki10,wiki10idle,youtube]
//!                 [--realism path\to\realism.exe] [--out bench]

#[path = "../procmem.rs"]
mod procmem;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

const WIKI: &str = "https://en.wikipedia.org/wiki/Web_browser";
const GOOGLE: &str = "https://www.google.com/";
const YOUTUBE: &str = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";

#[derive(Clone)]
struct Browser {
    key: String,
    label: String,
    exe: PathBuf,
}

#[derive(Clone)]
struct Scenario {
    key: &'static str,
    label: &'static str,
    urls: Vec<&'static str>,
    wait: Duration,
    cpu_window: Duration,
}

#[derive(Clone, Default, Debug)]
struct Sample {
    private_ws: u64,
    commit: u64,
    ws: u64,
    cpu_pct: f64,
    procs: usize,
    startup_ms: Option<u64>,
    ok: bool,
}

fn find_exe(cands: &[&str]) -> Option<PathBuf> {
    let la = std::env::var("LOCALAPPDATA").unwrap_or_default();
    for c in cands {
        let p = PathBuf::from(c.replace("%LOCALAPPDATA%", &la));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn browsers(realism: &Path) -> Vec<Browser> {
    let mut v = vec![
        Browser { key: "realism".into(), label: "Realism (Сбалансированный)".into(), exe: realism.to_path_buf() },
        Browser { key: "realism-saver".into(), label: "Realism (Экономия памяти)".into(), exe: realism.to_path_buf() },
    ];
    if let Some(p) = find_exe(&[r"C:\Program Files\Google\Chrome\Application\chrome.exe", r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe", r"%LOCALAPPDATA%\Google\Chrome\Application\chrome.exe"]) {
        v.push(Browser { key: "chrome".into(), label: "Google Chrome".into(), exe: p });
    }
    if let Some(p) = find_exe(&[r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"]) {
        v.push(Browser { key: "edge".into(), label: "Microsoft Edge".into(), exe: p });
    }
    if let Some(p) = find_exe(&[r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe", r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\Application\brave.exe"]) {
        v.push(Browser { key: "brave".into(), label: "Brave".into(), exe: p });
    }
    v
}

fn scenarios(settle: u64, idle_minutes: u64) -> Vec<Scenario> {
    let s = Duration::from_secs(settle);
    vec![
        Scenario { key: "idle", label: "Запуск без вкладок (стартовая страница)", urls: vec![], wait: Duration::from_secs(15), cpu_window: Duration::from_secs(10) },
        Scenario { key: "blank", label: "Одна вкладка about:blank", urls: vec!["about:blank"], wait: Duration::from_secs(15), cpu_window: Duration::from_secs(10) },
        Scenario { key: "google", label: "Одна вкладка Google", urls: vec![GOOGLE], wait: s, cpu_window: Duration::from_secs(10) },
        Scenario { key: "wiki5", label: "5 одинаковых сайтов (Wikipedia)", urls: vec![WIKI; 5], wait: s, cpu_window: Duration::from_secs(10) },
        Scenario { key: "wiki10", label: "10 одинаковых сайтов (Wikipedia)", urls: vec![WIKI; 10], wait: s + Duration::from_secs(5), cpu_window: Duration::from_secs(10) },
        Scenario {
            key: "wiki10idle",
            label: "10 вкладок после бездействия",
            urls: vec![WIKI; 10],
            wait: Duration::from_secs(idle_minutes * 60),
            cpu_window: Duration::from_secs(15),
        },
        Scenario { key: "youtube", label: "Воспроизведение видео YouTube", urls: vec![YOUTUBE], wait: Duration::from_secs(30), cpu_window: Duration::from_secs(20) },
    ]
}

fn launch(b: &Browser, profile: &Path, urls: &[&str]) -> std::io::Result<std::process::Child> {
    let mut c = Command::new(&b.exe);
    match b.key.as_str() {
        "realism" | "realism-saver" => {
            c.arg(format!("--profile-dir={}", profile.display()));
            c.arg("--bench");
            c.arg("--window-size=1280,800");
            c.arg(if b.key == "realism-saver" { "--mode=saver" } else { "--mode=balanced" });
        }
        _ => {
            c.arg(format!("--user-data-dir={}", profile.display()));
            c.args(["--no-first-run", "--no-default-browser-check", "--disable-sync", "--window-size=1280,800", "--window-position=0,0"]);
            c.arg("--autoplay-policy=no-user-gesture-required");
            c.arg("--mute-audio");
            c.arg("--new-window");
        }
    }
    for u in urls {
        c.arg(u);
    }
    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).stdin(std::process::Stdio::null());
    c.spawn()
}

struct FindCtx {
    pids: HashSet<u32>,
    found: Vec<HWND>,
}

unsafe extern "system" fn enum_cb(h: HWND, l: LPARAM) -> BOOL {
    unsafe {
        let ctx = &mut *(l.0 as *mut FindCtx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if ctx.pids.contains(&pid) && IsWindowVisible(h).as_bool() {
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(h, &mut r);
            if r.right - r.left > 200 && r.bottom - r.top > 150 {
                ctx.found.push(h);
            }
        }
        BOOL(1)
    }
}

fn visible_windows(pids: &HashSet<u32>) -> Vec<HWND> {
    let mut ctx = FindCtx { pids: pids.clone(), found: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut ctx as *mut FindCtx as isize));
    }
    ctx.found
}

fn tree_pids(root: u32, created: u64) -> HashSet<u32> {
    procmem::tree_known(root, created).into_iter().map(|p| p.pid).collect()
}

fn kill_tree(root: u32, created: u64) {
    // Polite close first (lets browsers flush), then terminate whatever is left.
    let pids = tree_pids(root, created);
    for h in visible_windows(&pids) {
        unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(1));
        }
    }
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(8) {
        if procmem::tree_known(root, created).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    for p in procmem::tree_known(root, created).into_iter().chain(pids.iter().map(|&pid| procmem::ProcEntry { pid, ppid: 0, name: String::new() })) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, p.pid) {
                let _ = TerminateProcess(h, 1);
                let _ = CloseHandle(h);
            }
        }
    }
    std::thread::sleep(Duration::from_secs(2));
}

fn median(v: &mut [u64]) -> u64 {
    v.sort();
    if v.is_empty() { 0 } else { v[v.len() / 2] }
}

fn run_one(b: &Browser, sc: &Scenario, tmp: &Path) -> Sample {
    let profile = tmp.join(format!("{}-{}-{}", b.key, sc.key, std::process::id() as u64 ^ procmem_now()));
    let _ = std::fs::create_dir_all(&profile);
    let start = Instant::now();
    let mut child = match launch(b, &profile, &sc.urls) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  ! failed to start {}: {e}", b.label);
            return Sample::default();
        }
    };
    let root = child.id();
    let created = procmem::times(root).map(|t| t.0).unwrap_or(0);
    // Startup: time until the first visible top-level window of the process tree.
    let mut startup_ms = None;
    while start.elapsed() < Duration::from_secs(30) {
        let pids = tree_pids(root, created);
        if !visible_windows(&pids).is_empty() {
            startup_ms = Some(start.elapsed().as_millis() as u64);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Wait (pages load, background tabs settle / go to sleep)
    let wait_until = start + sc.wait;
    while Instant::now() < wait_until {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(Some(_)) = child.try_wait() {
            if procmem::tree_known(root, created).is_empty() {
                break;
            }
        }
    }
    // CPU over a window, using per-process deltas of processes alive at both ends.
    let s0 = procmem::sample_known(root, created);
    let t0 = Instant::now();
    let mut mems = Vec::new();
    let mut commits = Vec::new();
    let mut wss = Vec::new();
    let mut procs = Vec::new();
    let n = 5u32;
    for i in 0..n {
        std::thread::sleep(sc.cpu_window / n);
        let s = if i + 1 == n { None } else { Some(procmem::sample_known(root, created)) };
        if let Some(s) = s {
            mems.push(s.total.private_ws);
            commits.push(s.total.commit);
            wss.push(s.total.ws);
            procs.push(s.procs.len() as u64);
        }
    }
    let s1 = procmem::sample_known(root, created);
    let el = t0.elapsed().as_secs_f64();
    mems.push(s1.total.private_ws);
    commits.push(s1.total.commit);
    wss.push(s1.total.ws);
    procs.push(s1.procs.len() as u64);
    let mut cpu_delta = 0u64;
    for (p, _, c1) in &s1.procs {
        if let Some((_, _, c0)) = s0.procs.iter().find(|x| x.0.pid == p.pid) {
            cpu_delta += c1.saturating_sub(*c0);
        } else {
            cpu_delta += *c1; // process started during the window
        }
    }
    let cpu_pct = cpu_delta as f64 / 1e7 / (el * procmem::cpu_count() as f64) * 100.0;
    let sample = Sample {
        private_ws: median(&mut mems),
        commit: median(&mut commits),
        ws: median(&mut wss),
        cpu_pct,
        procs: median(&mut procs) as usize,
        startup_ms,
        ok: !s1.procs.is_empty(),
    };
    kill_tree(root, created);
    let _ = child.wait();
    for _ in 0..10 {
        if std::fs::remove_dir_all(&profile).is_ok() || !profile.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    sample
}

fn procmem_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

fn say(s: &str) {
    use std::io::Write;
    let mut o = std::io::stdout();
    let _ = writeln!(o, "{s}");
    let _ = o.flush();
}

fn mb(v: u64) -> f64 {
    v as f64 / 1048576.0
}

fn main() {
    let mut runs = 3usize;
    let mut idle_runs = 2usize;
    let mut idle_minutes = 5u64;
    let mut settle = 20u64;
    let mut want_browsers: Option<Vec<String>> = None;
    let mut want_sc: Option<Vec<String>> = None;
    let mut out = PathBuf::from("bench");
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
    let mut realism = exe_dir.join("realism.exe");
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Diagnostic mode: realism-bench --probe <pid> <interval_s> <count>
    if args.first().map(|a| a == "--probe").unwrap_or(false) {
        let pid: u32 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        let every: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(30);
        let count: u32 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(5);
        for i in 0..count {
            let s = procmem::sample_tree(pid);
            let mut rows: Vec<String> = s.procs.iter().map(|(p, m, _)| format!("{}:{:.0}", p.pid, mb(m.private_ws))).collect();
            rows.sort();
            say(&format!("t={}s total={:.0}MB commit={:.0}MB  {}", i as u64 * every, mb(s.total.private_ws), mb(s.total.commit), rows.join(" ")));
            if i + 1 < count {
                std::thread::sleep(Duration::from_secs(every));
            }
        }
        return;
    }
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--runs" => runs = v.parse().unwrap_or(3),
            "--idle-runs" => idle_runs = v.parse().unwrap_or(2),
            "--idle-minutes" => idle_minutes = v.parse().unwrap_or(5),
            "--settle" => settle = v.parse().unwrap_or(20),
            "--browsers" => want_browsers = Some(v.split(',').map(String::from).collect()),
            "--scenarios" => want_sc = if v == "all" { None } else { Some(v.split(',').map(String::from).collect()) },
            "--realism" => realism = PathBuf::from(v),
            "--out" => out = PathBuf::from(v),
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let mut bs = browsers(&realism);
    if let Some(w) = &want_browsers {
        bs.retain(|b| w.contains(&b.key));
    }
    let mut scs = scenarios(settle, idle_minutes);
    if let Some(w) = &want_sc {
        scs.retain(|s| w.iter().any(|k| k == s.key));
    }
    let tmp = std::env::temp_dir().join("realism-bench");
    let _ = std::fs::create_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&out);
    let cpu_name = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default();
    say(&format!("realism-bench: {} browsers, {} scenarios, runs={runs} (idle scenario: {idle_runs}x{idle_minutes} min)", bs.len(), scs.len()));
    for b in &bs {
        say(&format!("  {:<28} {}", b.label, b.exe.display()));
    }
    // Warm-up: the first start of an executable is slowed by disk cache / antivirus scanning.
    // One unmeasured start per browser makes all measured runs comparable.
    let warm = Scenario { key: "warmup", label: "warmup", urls: vec!["about:blank"], wait: Duration::from_secs(8), cpu_window: Duration::from_secs(1) };
    for b in &bs {
        say(&format!("warm-up: {}", b.label));
        let _ = run_one(b, &warm, &tmp);
    }
    // results[scenario][browser] = Vec<Sample>
    let mut results: BTreeMap<String, BTreeMap<String, Vec<Sample>>> = BTreeMap::new();
    let mut raw = String::from("scenario,browser,run,private_ws_mb,commit_mb,ws_mb,cpu_pct,processes,startup_ms\n");
    let max_runs = runs.max(idle_runs);
    for run in 1..=max_runs {
        for sc in &scs {
            let n = if sc.key == "wiki10idle" { idle_runs } else { runs };
            if run > n {
                continue;
            }
            for b in &bs {
                let s = run_one(b, sc, &tmp);
                say(&format!(
                    "[run {run}] {:<38} {:<28} {:>7.1} MB  commit {:>7.1} MB  CPU {:>5.2}%  procs {:>2}  start {} ms",
                    sc.label,
                    b.label,
                    mb(s.private_ws),
                    mb(s.commit),
                    s.cpu_pct,
                    s.procs,
                    s.startup_ms.map(|v| v.to_string()).unwrap_or("-".into())
                ));
                raw.push_str(&format!(
                    "{},{},{run},{:.1},{:.1},{:.1},{:.3},{},{}\n",
                    sc.key,
                    b.key,
                    mb(s.private_ws),
                    mb(s.commit),
                    mb(s.ws),
                    s.cpu_pct,
                    s.procs,
                    s.startup_ms.map(|v| v.to_string()).unwrap_or_default()
                ));
                results.entry(sc.key.to_string()).or_default().entry(b.key.clone()).or_default().push(s);
                let _ = std::fs::write(out.join("raw.csv"), &raw);
            }
        }
    }
    // ---------------- report
    let mut md = String::new();
    md.push_str("# Realism Browser — сравнение потребления ресурсов\n\n");
    md.push_str(&format!(
        "Машина: {cpu_name}, {} логических ядер. Каждый запуск — новый временный профиль, окно 1280×800, одинаковые URL и время ожидания. \
         Память — сумма по **всему дереву процессов** браузера. Значения — медиана по запускам (в скобках min–max).\n\n",
        procmem::cpu_count()
    ));
    md.push_str("* **RAM** — private working set (столбец «Память» в Диспетчере задач), МБ\n* **Commit** — выделенная приватная память, МБ\n* **CPU** — средняя загрузка всех ядер за окно измерения, %\n* **Запуск** — время до появления окна, мс\n\n");
    let names: BTreeMap<String, String> = bs.iter().map(|b| (b.key.clone(), b.label.clone())).collect();
    let med = |v: &Vec<Sample>, f: &dyn Fn(&Sample) -> f64| -> (f64, f64, f64) {
        let mut x: Vec<f64> = v.iter().filter(|s| s.ok).map(f).collect();
        x.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if x.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        (x[x.len() / 2], x[0], x[x.len() - 1])
    };
    for sc in &scs {
        let Some(per) = results.get(sc.key) else { continue };
        md.push_str(&format!("## {}\n\n", sc.label));
        let refs: Vec<(&str, f64)> = ["chrome", "edge", "brave"]
            .iter()
            .filter_map(|k| per.get(*k).map(|v| (*k, med(v, &|s| mb(s.private_ws)).0)))
            .collect();
        md.push_str("| Браузер | RAM, МБ | Commit, МБ | CPU, % | Запуск, мс | Процессов |");
        for (k, _) in &refs {
            md.push_str(&format!(" vs {} |", names.get(*k).cloned().unwrap_or_default()));
        }
        md.push_str("\n|---|---:|---:|---:|---:|---:|");
        for _ in &refs {
            md.push_str("---:|");
        }
        md.push('\n');
        for b in &bs {
            let Some(v) = per.get(&b.key) else { continue };
            let (r, rmin, rmax) = med(v, &|s| mb(s.private_ws));
            let (c, _, _) = med(v, &|s| mb(s.commit));
            let (cpu, _, _) = med(v, &|s| s.cpu_pct);
            let (st, _, _) = med(v, &|s| s.startup_ms.unwrap_or(0) as f64);
            let (pc, _, _) = med(v, &|s| s.procs as f64);
            md.push_str(&format!("| {} | **{:.0}** ({:.0}–{:.0}) | {:.0} | {:.2} | {:.0} | {:.0} |", b.label, r, rmin, rmax, c, cpu, st, pc));
            for (k, refv) in &refs {
                if *k == b.key.as_str() || *refv <= 0.0 {
                    md.push_str(" — |");
                } else {
                    let d = (r - refv) / refv * 100.0;
                    md.push_str(&format!(" {}{:.0}% |", if d > 0.0 { "+" } else { "" }, d));
                }
            }
            md.push('\n');
        }
        md.push('\n');
    }
    md.push_str("Отрицательный процент — Realism потребляет меньше RAM, чем браузер в заголовке столбца.\n\nСырые данные: `raw.csv`.\n");
    let _ = std::fs::write(out.join("results.md"), &md);
    println!("\n{md}");
    say(&format!("Saved to {}", out.display()));
}
