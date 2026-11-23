//! Process-tree memory and CPU measurement (shared by the browser and realism-bench).
//!
//! Metrics per process:
//! * `private_ws` - private working set (what Task Manager shows in the "Memory" column)
//! * `ws`         - full working set (includes shared pages, double-counts DLLs across processes)
//! * `commit`     - private bytes / commit charge

#![allow(dead_code)]

use std::collections::HashMap;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

#[derive(Clone, Debug)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Mem {
    pub private_ws: u64,
    pub ws: u64,
    pub commit: u64,
}

impl std::ops::AddAssign for Mem {
    fn add_assign(&mut self, o: Mem) {
        self.private_ws += o.private_ws;
        self.ws += o.ws;
        self.commit += o.commit;
    }
}

#[repr(C)]
#[derive(Default)]
#[allow(non_snake_case)]
struct PmcEx2 {
    cb: u32,
    PageFaultCount: u32,
    PeakWorkingSetSize: usize,
    WorkingSetSize: usize,
    QuotaPeakPagedPoolUsage: usize,
    QuotaPagedPoolUsage: usize,
    QuotaPeakNonPagedPoolUsage: usize,
    QuotaNonPagedPoolUsage: usize,
    PagefileUsage: usize,
    PeakPagefileUsage: usize,
    PrivateUsage: usize,
    PrivateWorkingSetSize: usize,
    SharedCommitUsage: u64,
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(pid: u32) -> Option<Handle> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid).ok().map(Handle) }
}

fn open_limited(pid: u32) -> Option<Handle> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok().map(Handle) }
}

pub fn all_processes() -> Vec<ProcEntry> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let snap = Handle(snap);
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap.0, &mut e).is_ok() {
            loop {
                out.push(ProcEntry {
                    pid: e.th32ProcessID,
                    ppid: e.th32ParentProcessID,
                    name: crate_from_wide(&e.szExeFile),
                });
                if Process32NextW(snap.0, &mut e).is_err() {
                    break;
                }
            }
        }
    }
    out
}

fn crate_from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// (creation time, cpu time) in 100ns units.
pub fn times(pid: u32) -> Option<(u64, u64)> {
    let h = open_limited(pid)?;
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(h.0, &mut c, &mut e, &mut k, &mut u).ok()? };
    Some((ft(c), ft(k) + ft(u)))
}

pub fn memory(pid: u32) -> Option<Mem> {
    let h = open(pid)?;
    let mut m = PmcEx2 { cb: std::mem::size_of::<PmcEx2>() as u32, ..Default::default() };
    unsafe {
        let ok = GetProcessMemoryInfo(h.0, &mut m as *mut PmcEx2 as *mut PROCESS_MEMORY_COUNTERS, m.cb).is_ok();
        if ok {
            return Some(Mem { private_ws: m.PrivateWorkingSetSize as u64, ws: m.WorkingSetSize as u64, commit: m.PrivateUsage as u64 });
        }
        // Older Windows: no private working set; fall back to EX (private bytes) as the estimate.
        let mut m = PmcEx2 { cb: (std::mem::size_of::<PmcEx2>() - 16) as u32, ..Default::default() };
        GetProcessMemoryInfo(h.0, &mut m as *mut PmcEx2 as *mut PROCESS_MEMORY_COUNTERS, m.cb).ok()?;
        Some(Mem { private_ws: m.PrivateUsage as u64, ws: m.WorkingSetSize as u64, commit: m.PrivateUsage as u64 })
    }
}

/// All descendants of `root` (inclusive). Guards against PID reuse by checking creation times.
pub fn tree(root: u32) -> Vec<ProcEntry> {
    tree_from(&all_processes(), root)
}

/// Like `tree`, but still finds the descendants if the root process has already exited
/// (e.g. a launcher stub that spawned the real browser process).
pub fn tree_known(root: u32, root_created: u64) -> Vec<ProcEntry> {
    let all = all_processes();
    if all.iter().any(|p| p.pid == root) {
        return tree_from(&all, root);
    }
    let mut out = Vec::new();
    let mut stack = vec![(root, root_created)];
    let mut seen = std::collections::HashSet::new();
    while let Some((pid, created)) = stack.pop() {
        for c in all.iter().filter(|p| p.ppid == pid && p.pid != pid) {
            if !seen.insert(c.pid) {
                continue;
            }
            let ct = times(c.pid).map(|t| t.0).unwrap_or(u64::MAX);
            if ct != u64::MAX && ct + 10_000_000 < created {
                continue;
            }
            out.push(c.clone());
            stack.push((c.pid, ct));
        }
    }
    out
}

pub fn tree_from(all: &[ProcEntry], root: u32) -> Vec<ProcEntry> {
    let mut children: HashMap<u32, Vec<&ProcEntry>> = HashMap::new();
    for p in all {
        if p.pid != p.ppid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    let Some(rootp) = all.iter().find(|p| p.pid == root) else { return out };
    let mut created: HashMap<u32, u64> = HashMap::new();
    created.insert(root, times(root).map(|t| t.0).unwrap_or(0));
    out.push(rootp.clone());
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        let parent_created = *created.get(&pid).unwrap_or(&0);
        if let Some(ch) = children.get(&pid) {
            for c in ch {
                if out.iter().any(|o| o.pid == c.pid) {
                    continue;
                }
                let ct = times(c.pid).map(|t| t.0).unwrap_or(u64::MAX);
                // A child must have been created after its parent; otherwise the PID was reused.
                if ct != u64::MAX && ct + 10_000_000 < parent_created {
                    continue;
                }
                created.insert(c.pid, ct);
                out.push((*c).clone());
                stack.push(c.pid);
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct TreeSample {
    pub procs: Vec<(ProcEntry, Mem, u64)>, // entry, memory, cpu time (100ns)
    pub total: Mem,
    pub cpu_100ns: u64,
}

pub fn sample_tree(root: u32) -> TreeSample {
    sample_list(tree(root))
}

pub fn sample_known(root: u32, created: u64) -> TreeSample {
    sample_list(tree_known(root, created))
}

fn sample_list(list: Vec<ProcEntry>) -> TreeSample {
    let mut s = TreeSample::default();
    for p in list {
        let m = memory(p.pid).unwrap_or_default();
        let cpu = times(p.pid).map(|t| t.1).unwrap_or(0);
        s.total += m;
        s.cpu_100ns += cpu;
        s.procs.push((p, m, cpu));
    }
    s
}

pub fn cpu_count() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}
