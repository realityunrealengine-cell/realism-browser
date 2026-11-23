//! Storage on top of the SQLite that ships with Windows (winsqlite3.dll).
//! No C compiler or bundled SQLite needed, and zero bytes added to the binary.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;

#[link(name = "winsqlite3", kind = "raw-dylib")]
unsafe extern "system" {
    fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut c_void, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close_v2(db: *mut c_void) -> c_int;
    fn sqlite3_exec(db: *mut c_void, sql: *const c_char, cb: *const c_void, arg: *mut c_void, err: *mut *mut c_char) -> c_int;
    fn sqlite3_prepare_v2(db: *mut c_void, sql: *const c_char, n: c_int, stmt: *mut *mut c_void, tail: *mut *const c_char) -> c_int;
    fn sqlite3_step(stmt: *mut c_void) -> c_int;
    fn sqlite3_finalize(stmt: *mut c_void) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut c_void, i: c_int, v: i64) -> c_int;
    fn sqlite3_bind_double(stmt: *mut c_void, i: c_int, v: f64) -> c_int;
    fn sqlite3_bind_text(stmt: *mut c_void, i: c_int, p: *const c_char, n: c_int, d: isize) -> c_int;
    fn sqlite3_bind_blob(stmt: *mut c_void, i: c_int, p: *const c_void, n: c_int, d: isize) -> c_int;
    fn sqlite3_bind_null(stmt: *mut c_void, i: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut c_void, i: c_int) -> i64;
    fn sqlite3_column_double(stmt: *mut c_void, i: c_int) -> f64;
    fn sqlite3_column_text(stmt: *mut c_void, i: c_int) -> *const u8;
    fn sqlite3_column_blob(stmt: *mut c_void, i: c_int) -> *const c_void;
    fn sqlite3_column_bytes(stmt: *mut c_void, i: c_int) -> c_int;
    fn sqlite3_errmsg(db: *mut c_void) -> *const c_char;
    fn sqlite3_last_insert_rowid(db: *mut c_void) -> i64;
    fn sqlite3_changes(db: *mut c_void) -> c_int;
    fn sqlite3_busy_timeout(db: *mut c_void, ms: c_int) -> c_int;
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_TRANSIENT: isize = -1;
const OPEN_FLAGS: c_int = 0x2 | 0x4 | 0x10000; // READWRITE | CREATE | FULLMUTEX

pub enum V<'a> {
    I(i64),
    F(f64),
    T(&'a str),
    B(&'a [u8]),
    N,
}

pub struct Row(*mut c_void);

impl Row {
    pub fn int(&self, i: i32) -> i64 {
        unsafe { sqlite3_column_int64(self.0, i) }
    }
    pub fn f64(&self, i: i32) -> f64 {
        unsafe { sqlite3_column_double(self.0, i) }
    }
    pub fn text(&self, i: i32) -> String {
        unsafe {
            let p = sqlite3_column_text(self.0, i);
            if p.is_null() {
                return String::new();
            }
            let n = sqlite3_column_bytes(self.0, i) as usize;
            String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned()
        }
    }
    pub fn blob(&self, i: i32) -> Vec<u8> {
        unsafe {
            let p = sqlite3_column_blob(self.0, i);
            if p.is_null() {
                return Vec::new();
            }
            let n = sqlite3_column_bytes(self.0, i) as usize;
            std::slice::from_raw_parts(p as *const u8, n).to_vec()
        }
    }
}

pub struct Db {
    h: *mut c_void,
}

impl Drop for Db {
    fn drop(&mut self) {
        unsafe {
            sqlite3_close_v2(self.h);
        }
    }
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let mut h = std::ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(c.as_ptr(), &mut h, OPEN_FLAGS, std::ptr::null()) };
        let db = Db { h };
        if rc != SQLITE_OK {
            return Err(db.err());
        }
        unsafe { sqlite3_busy_timeout(h, 2000) };
        db.exec(SCHEMA)?;
        Ok(db)
    }

    pub fn open_memory() -> Db {
        Db::open(Path::new(":memory:")).expect("in-memory sqlite")
    }

    fn err(&self) -> String {
        unsafe {
            let p = sqlite3_errmsg(self.h);
            if p.is_null() { "sqlite error".into() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
        }
    }

    pub fn exec(&self, sql: &str) -> Result<(), String> {
        let c = CString::new(sql).map_err(|e| e.to_string())?;
        let rc = unsafe { sqlite3_exec(self.h, c.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if rc != SQLITE_OK { Err(self.err()) } else { Ok(()) }
    }

    fn prepare(&self, sql: &str, params: &[V]) -> Option<*mut c_void> {
        let c = CString::new(sql).ok()?;
        let mut st = std::ptr::null_mut();
        let rc = unsafe { sqlite3_prepare_v2(self.h, c.as_ptr(), -1, &mut st, std::ptr::null_mut()) };
        if rc != SQLITE_OK || st.is_null() {
            eprintln!("sqlite prepare failed: {} :: {sql}", self.err());
            return None;
        }
        for (i, p) in params.iter().enumerate() {
            let i = i as c_int + 1;
            unsafe {
                match p {
                    V::I(v) => sqlite3_bind_int64(st, i, *v),
                    V::F(v) => sqlite3_bind_double(st, i, *v),
                    V::T(s) => sqlite3_bind_text(st, i, s.as_ptr() as *const c_char, s.len() as c_int, SQLITE_TRANSIENT),
                    V::B(b) => sqlite3_bind_blob(st, i, b.as_ptr() as *const c_void, b.len() as c_int, SQLITE_TRANSIENT),
                    V::N => sqlite3_bind_null(st, i),
                };
            }
        }
        Some(st)
    }

    /// Execute a statement, returns number of changed rows.
    pub fn run(&self, sql: &str, params: &[V]) -> usize {
        let Some(st) = self.prepare(sql, params) else { return 0 };
        unsafe {
            let mut rc = sqlite3_step(st);
            while rc == SQLITE_ROW {
                rc = sqlite3_step(st);
            }
            if rc != SQLITE_DONE {
                eprintln!("sqlite step failed: {} :: {sql}", self.err());
            }
            sqlite3_finalize(st);
            sqlite3_changes(self.h) as usize
        }
    }

    pub fn query<T>(&self, sql: &str, params: &[V], mut f: impl FnMut(&Row) -> T) -> Vec<T> {
        let mut out = Vec::new();
        let Some(st) = self.prepare(sql, params) else { return out };
        unsafe {
            while sqlite3_step(st) == SQLITE_ROW {
                out.push(f(&Row(st)));
            }
            sqlite3_finalize(st);
        }
        out
    }

    pub fn last_id(&self) -> i64 {
        unsafe { sqlite3_last_insert_rowid(self.h) }
    }

    // ---------- settings ----------
    pub fn get_setting(&self, k: &str) -> Option<String> {
        self.query("SELECT v FROM settings WHERE k=?", &[V::T(k)], |r| r.text(0)).into_iter().next()
    }
    pub fn set_setting(&self, k: &str, v: &str) {
        self.run("INSERT INTO settings(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", &[V::T(k), V::T(v)]);
    }

    // ---------- history ----------
    pub fn add_visit(&self, url: &str, title: &str) {
        let now = crate::util::now_ms();
        self.run(
            "INSERT INTO history(url,title,visits,last_visit) VALUES(?,?,1,?) \
             ON CONFLICT(url) DO UPDATE SET visits=visits+1, last_visit=excluded.last_visit, \
             title=CASE WHEN excluded.title<>'' THEN excluded.title ELSE title END",
            &[V::T(url), V::T(title), V::I(now)],
        );
    }
    pub fn update_title(&self, url: &str, title: &str) {
        if !title.is_empty() {
            self.run("UPDATE history SET title=? WHERE url=?", &[V::T(title), V::T(url)]);
        }
    }

    // ---------- zoom ----------
    pub fn zoom_for(&self, host: &str) -> Option<f64> {
        self.query("SELECT factor FROM zoom WHERE host=?", &[V::T(host)], |r| r.f64(0)).into_iter().next()
    }
    pub fn set_zoom(&self, host: &str, f: f64) {
        if (f - 1.0).abs() < 0.001 {
            self.run("DELETE FROM zoom WHERE host=?", &[V::T(host)]);
        } else {
            self.run("INSERT INTO zoom(host,factor) VALUES(?,?) ON CONFLICT(host) DO UPDATE SET factor=excluded.factor", &[V::T(host), V::F(f)]);
        }
    }

    // ---------- permissions ----------
    pub fn permission(&self, origin: &str, kind: i32) -> Option<i32> {
        self.query("SELECT state FROM permissions WHERE origin=? AND kind=?", &[V::T(origin), V::I(kind as i64)], |r| r.int(0) as i32)
            .into_iter()
            .next()
    }
    pub fn set_permission(&self, origin: &str, kind: i32, state: i32) {
        self.run(
            "INSERT INTO permissions(origin,kind,state) VALUES(?,?,?) ON CONFLICT(origin,kind) DO UPDATE SET state=excluded.state",
            &[V::T(origin), V::I(kind as i64), V::I(state as i64)],
        );
    }
}

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA cache_size=-512;
CREATE TABLE IF NOT EXISTS settings(k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS history(id INTEGER PRIMARY KEY, url TEXT UNIQUE NOT NULL, title TEXT NOT NULL DEFAULT '', visits INTEGER NOT NULL DEFAULT 1, last_visit INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS history_time ON history(last_visit);
CREATE TABLE IF NOT EXISTS bookmarks(id INTEGER PRIMARY KEY, parent INTEGER NOT NULL, is_folder INTEGER NOT NULL DEFAULT 0, title TEXT NOT NULL DEFAULT '', url TEXT NOT NULL DEFAULT '', pos INTEGER NOT NULL DEFAULT 0, added INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS bookmarks_parent ON bookmarks(parent, pos);
INSERT OR IGNORE INTO bookmarks(id,parent,is_folder,title,pos) VALUES(1,0,1,'Панель закладок',0);
INSERT OR IGNORE INTO bookmarks(id,parent,is_folder,title,pos) VALUES(2,0,1,'Другие закладки',1);
CREATE TABLE IF NOT EXISTS session(win INTEGER NOT NULL, pos INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL, active INTEGER NOT NULL, icon BLOB, back TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS downloads(id INTEGER PRIMARY KEY, url TEXT NOT NULL, path TEXT NOT NULL, total INTEGER NOT NULL, received INTEGER NOT NULL, state TEXT NOT NULL, ts INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS permissions(origin TEXT NOT NULL, kind INTEGER NOT NULL, state INTEGER NOT NULL, PRIMARY KEY(origin, kind));
CREATE TABLE IF NOT EXISTS popups_allowed(origin TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS zoom(host TEXT PRIMARY KEY, factor REAL NOT NULL);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let db = Db::open_memory();
        db.set_setting("a", "1");
        db.set_setting("a", "2");
        assert_eq!(db.get_setting("a").as_deref(), Some("2"));
        db.add_visit("https://x.y/", "X");
        db.add_visit("https://x.y/", "");
        let v = db.query("SELECT visits, title FROM history", &[], |r| (r.int(0), r.text(1)));
        assert_eq!(v, vec![(2, "X".to_string())]);
        let roots = db.query("SELECT COUNT(*) FROM bookmarks WHERE parent=0", &[], |r| r.int(0));
        assert_eq!(roots[0], 2);
        db.set_zoom("x.y", 1.25);
        assert_eq!(db.zoom_for("x.y"), Some(1.25));
        db.set_zoom("x.y", 1.0);
        assert_eq!(db.zoom_for("x.y"), None);
    }
}
