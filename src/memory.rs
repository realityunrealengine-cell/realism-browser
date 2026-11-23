//! Smart Tab Sleeping, engine idle shutdown, performance data for realism://performance.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::app::{App, DlState, EnvState, Mode};
use crate::events::ProcInfo;
use crate::procmem;
use crate::util::*;
use crate::webview;

impl App {
    /// Why a background tab must not be frozen. Ok(()) = may be suspended.
    pub fn can_suspend(&self, wi: usize, ti: usize) -> Result<(), &'static str> {
        let w = &self.windows[wi];
        let t = &w.tabs[ti];
        if t.audio {
            return Err("воспроизводит звук");
        }
        if t.media_active {
            return Err("использует камеру/микрофон");
        }
        if w.fullscreen.is_some() && w.active == ti {
            return Err("полноэкранный режим");
        }
        if self.downloads.iter().any(|d| d.tab == t.id && matches!(d.state, DlState::Running | DlState::Paused)) {
            return Err("идёт загрузка файла");
        }
        if !t.perms.is_empty() {
            return Err("ожидает ответа на запрос разрешения");
        }
        let host = host_of(&t.url).trim_start_matches("www.").to_ascii_lowercase();
        if !host.is_empty() && self.s.exceptions.iter().any(|e| {
            let e = e.trim().trim_start_matches("www.").to_ascii_lowercase();
            !e.is_empty() && (host == e || host.ends_with(&format!(".{e}")))
        }) {
            return Err("сайт в списке исключений");
        }
        Ok(())
    }

    /// Why a background tab must not be unloaded. Ok(()) = may be discarded.
    pub fn can_discard(&self, wi: usize, ti: usize) -> Result<(), &'static str> {
        self.can_suspend(wi, ti)?;
        let w = &self.windows[wi];
        let t = &w.tabs[ti];
        if t.dirty_input {
            return Err("есть несохранённый ввод в форме");
        }
        if w.private {
            // Unloading every private tab would end the private session (logouts).
            return Err("приватное окно");
        }
        if w.active == ti {
            return Err("активная вкладка");
        }
        if t.loading && t.created.elapsed() < Duration::from_secs(60) {
            return Err("загружается");
        }
        Ok(())
    }

    /// Periodic housekeeping (every 10 s): RAM indicator, tab sleeping, engine idle shutdown.
    pub fn tick(&mut self) {
        self.auto_update_check();
        let any_visible = self.windows.iter().any(|w| !w.minimized);
        if any_visible {
            let s = procmem::sample_tree(std::process::id());
            let new = s.total.private_ws;
            let changed = (new as i64 - self.ram_total as i64).unsigned_abs() > 512 * 1024;
            self.ram_total = new;
            if changed {
                for w in &self.windows {
                    w.invalidate();
                }
            }
        }
        self.sleep_pass(false);
        self.engine_idle_check();
    }

    pub fn engine_idle_check(&mut self) {
        let pol = self.policy();
        if !matches!(self.env, EnvState::Ready(_)) || self.live_webviews() > 0 || self.windows.iter().any(|w| w.editing) {
            self.env_idle_since = None;
            return;
        }
        let Some(limit) = pol.env_idle else { return };
        let since = *self.env_idle_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= limit {
            // No webview left: let the whole WebView2 process tree exit.
            self.env = EnvState::None;
            self.env_idle_since = None;
        }
    }

    /// Applies the sleeping policy. `force` = "free memory now" (ignores timers).
    pub fn sleep_pass(&mut self, force: bool) -> usize {
        let pol = self.policy();
        let now = Instant::now();
        let mut discard: Vec<u64> = Vec::new();
        let mut live_bg: Vec<(Instant, usize, usize)> = Vec::new();
        let mut acted = 0usize;
        for wi in 0..self.windows.len() {
            for ti in 0..self.windows[wi].tabs.len() {
                let w = &self.windows[wi];
                let t = &w.tabs[ti];
                let visible = w.active == ti && !w.minimized;
                if visible || t.wv.is_none() {
                    continue;
                }
                let idle = t.hidden_since.map(|h| now.duration_since(h)).unwrap_or_default();
                let can_s = self.can_suspend(wi, ti).is_ok();
                let can_d = self.can_discard(wi, ti).is_ok();
                if let Some(d) = pol.discard_after {
                    if can_d && (force || idle >= d) {
                        discard.push(t.id);
                        continue;
                    }
                } else if force && can_d {
                    discard.push(t.id);
                    continue;
                }
                let id = t.id;
                let suspended = t.suspended;
                let low = t.low_mem;
                let last = t.last_active;
                if can_s {
                    let never_shown = !self.windows[wi].tabs[ti].shown;
                    let core = self.windows[wi].tabs[ti].wv.as_ref().unwrap().core.clone();
                    // Never lower the memory target of a page that is still loading (it would slow the load down).
                    let loading = self.windows[wi].tabs[ti].loading;
                    let low_due = !loading && (pol.low_mem_on_hide || pol.low_mem_after.map(|d| idle >= d).unwrap_or(false));
                    if (low_due || force || idle >= pol.suspend_after) && !low {
                        webview::set_low_memory(&core, true);
                        self.windows[wi].tabs[ti].low_mem = true;
                        acted += 1;
                    }
                    let backoff = self.windows[wi].tabs[ti].suspend_failed.map(|t| t.elapsed() < Duration::from_secs(60)).unwrap_or(false);
                    if !suspended && (force || (idle >= pol.suspend_after && !backoff)) {
                        if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                            webview::try_suspend(id, wv, never_shown);
                        }
                        acted += 1;
                    }
                }
                if can_d {
                    live_bg.push((last, wi, ti));
                }
            }
        }
        // LRU cap on live background tabs.
        let live_total = self.windows.iter().flat_map(|w| w.tabs.iter().enumerate().map(move |(i, t)| (w, i, t))).filter(|(w, i, t)| t.wv.is_some() && !(w.active == *i && !w.minimized)).count();
        if live_total.saturating_sub(discard.len()) > pol.max_live_bg {
            live_bg.sort_by_key(|x| x.0);
            let mut excess = live_total - discard.len() - pol.max_live_bg;
            for (_, wi, ti) in live_bg {
                if excess == 0 {
                    break;
                }
                let t = &self.windows[wi].tabs[ti];
                let idle = t.hidden_since.map(|h| now.duration_since(h)).unwrap_or_default();
                if idle >= Duration::from_secs(20) && !discard.contains(&t.id) {
                    discard.push(t.id);
                    excess -= 1;
                }
            }
        }
        for id in discard {
            if let Some((wi, ti)) = self.loc(id) {
                self.discard(wi, ti);
                acted += 1;
            }
        }
        if acted > 0 {
            self.redraw_all();
        }
        acted
    }

    pub fn free_memory(&mut self) -> usize {
        let n = self.sleep_pass(true);
        for w in self.windows.iter_mut() {
            // Drop the off-screen chrome buffers; they are recreated (small) on next paint.
            w.canvas = None;
        }
        if let Some(p) = self.popup.as_mut() {
            p.canvas = None;
        }
        self.redraw_all();
        n
    }

    pub fn set_mode(&mut self, m: Mode) {
        self.s.mode = m;
        self.db.set_setting("mode", m.key());
        let pol = m.policy();
        if !pol.low_mem_on_hide {
            // Leaving memory-saver: background tabs go back to the normal memory target.
            for w in &mut self.windows {
                for t in &mut w.tabs {
                    if t.low_mem && !t.suspended {
                        if let Some(wv) = &t.wv {
                            webview::set_low_memory(&wv.core, false);
                        }
                        t.low_mem = false;
                    }
                }
            }
        }
        if pol.prewarm {
            self.start_env();
        }
        self.sleep_pass(false);
        self.redraw_all();
    }

    /// True if the running engine was started with different process settings than the current mode wants.
    pub fn engine_restart_needed(&self) -> bool {
        {
        let (old, new) = (self.env_mode.policy(), self.policy());
        matches!(self.env, EnvState::Ready(_)) && (old.process_per_site != new.process_per_site || old.prewarm != new.prewarm)
    }
    }

    pub fn begin_perf(&mut self, resp: wry::RequestAsyncResponder) {
        let req = self.new_id();
        self.pending_api.insert(req, (resp, "perf".into()));
        let ok = match &self.env {
            EnvState::Ready(env) => webview::query_processes(env, req),
            _ => false,
        };
        if !ok {
            self.finish_perf(req, None);
        }
    }

    pub fn finish_perf(&mut self, req: u64, infos: Option<Vec<ProcInfo>>) {
        let Some((resp, _)) = self.pending_api.remove(&req) else { return };
        let me = std::process::id();
        let sample = procmem::sample_tree(me);
        let now = Instant::now();
        let ncpu = procmem::cpu_count() as f64;
        let elapsed = self.cpu_prev_at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        let mut cpu_map = HashMap::new();
        let mut total_cpu = 0.0f64;
        let kinds: HashMap<u32, i32> = infos.as_ref().map(|v| v.iter().map(|p| (p.pid, p.kind)).collect()).unwrap_or_default();
        let mut procs = Vec::new();
        for (p, m, cpu) in &sample.procs {
            let pct = match (self.cpu_prev.get(&p.pid), elapsed > 0.2) {
                (Some(prev), true) => ((cpu.saturating_sub(*prev)) as f64 / 1e7) / (elapsed * ncpu) * 100.0,
                _ => 0.0,
            };
            total_cpu += pct;
            cpu_map.insert(p.pid, *cpu);
            let kind = if p.pid == me {
                "Интерфейс Realism (Rust)"
            } else {
                match kinds.get(&p.pid) {
                    Some(0) => "Браузер WebView2",
                    Some(1) => "Рендерер (страницы)",
                    Some(2) => "Служебный (сеть/аудио/хранилище)",
                    Some(3) => "Sandbox helper",
                    Some(4) => "GPU",
                    Some(5) | Some(6) => "Плагин",
                    _ => "Служебный (crashpad)",
                }
            };
            procs.push(json!({
                "pid": p.pid, "name": p.name, "kind": kind,
                "private_ws": m.private_ws, "ws": m.ws, "commit": m.commit,
                "cpu": (pct * 10.0).round() / 10.0,
            }));
        }
        self.cpu_prev = cpu_map;
        self.cpu_prev_at = Some(now);
        // Map renderer processes to tabs via the main frame id of each webview.
        let mut frame_to_tab: HashMap<u32, u64> = HashMap::new();
        for w in &self.windows {
            for t in &w.tabs {
                if let Some(wv) = &t.wv {
                    if wv.frame_id != 0 {
                        frame_to_tab.insert(wv.frame_id, t.id);
                    }
                }
            }
        }
        let mut tab_mem: HashMap<u64, (u64, usize)> = HashMap::new();
        if let Some(infos) = &infos {
            for p in infos.iter().filter(|p| p.kind == 1) {
                let mut tabs: Vec<u64> = p.frames.iter().filter_map(|f| frame_to_tab.get(f).copied()).collect();
                tabs.sort();
                tabs.dedup();
                if tabs.is_empty() {
                    continue;
                }
                let mem = sample.procs.iter().find(|x| x.0.pid == p.pid).map(|x| x.1.private_ws).unwrap_or(0);
                let share = mem / tabs.len() as u64;
                for t in &tabs {
                    let e = tab_mem.entry(*t).or_insert((0, 0));
                    e.0 += share;
                    e.1 = e.1.max(tabs.len());
                }
            }
        }
        let mut tabs = Vec::new();
        let (mut n_live, mut n_susp, mut n_disc, mut n_ntp) = (0, 0, 0, 0);
        for (wi, w) in self.windows.iter().enumerate() {
            for (ti, t) in w.tabs.iter().enumerate() {
                let state = if t.is_ntp() {
                    n_ntp += 1;
                    "ntp"
                } else if t.wv.is_none() {
                    n_disc += 1;
                    "discarded"
                } else if t.suspended {
                    n_susp += 1;
                    "suspended"
                } else if w.active == ti && !w.minimized {
                    n_live += 1;
                    "active"
                } else {
                    n_live += 1;
                    "background"
                };
                let reason = if t.wv.is_some() && !(w.active == ti && !w.minimized) { self.can_discard(wi, ti).err() } else { None };
                let mem = tab_mem.get(&t.id);
                tabs.push(json!({
                    "id": t.id, "title": t.display_title(), "url": display_url(&t.url), "state": state,
                    "private": w.private, "audio": t.audio,
                    "mem": mem.map(|m| m.0), "shared": mem.map(|m| m.1).unwrap_or(0),
                    "protected": reason,
                    "idle": t.hidden_since.map(|h| h.elapsed().as_secs()).unwrap_or(0),
                }));
            }
        }
        let pol = self.policy();
        let body = json!({
            "mode": self.s.mode.key(),
            "mode_label": self.s.mode.label(),
            "engine": match &self.env { EnvState::Ready(_) => "running", EnvState::Creating => "starting", EnvState::None => "stopped" },
            "engine_restart_needed": self.engine_restart_needed(),
            "per_tab_available": infos.is_some(),
            "total": { "private_ws": sample.total.private_ws, "ws": sample.total.ws, "commit": sample.total.commit },
            "cpu": (total_cpu * 10.0).round() / 10.0,
            "cpus": ncpu,
            "proc_count": sample.procs.len(),
            "processes": procs,
            "tabs": tabs,
            "counts": { "live": n_live, "suspended": n_susp, "discarded": n_disc, "ntp": n_ntp },
            "policy": {
                "suspend_after": pol.suspend_after.as_secs(),
                "discard_after": pol.discard_after.map(|d| d.as_secs()),
                "max_live_bg": if pol.max_live_bg == usize::MAX { None } else { Some(pol.max_live_bg) },
                "low_mem_on_hide": pol.low_mem_on_hide,
                "low_mem_after": pol.low_mem_after.map(|d| d.as_secs()),
                "engine_idle": pol.env_idle.map(|d| d.as_secs()),
                "process_per_site": pol.process_per_site,
            },
        });
        self.ram_total = sample.total.private_ws;
        crate::pages::respond_json(resp, &body);
    }
}
