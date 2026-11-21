// SPDX-License-Identifier: Apache-2.0
//!
//! SigmaOS-recovery — recovery environment manager.
//!
//! Provides a minimal recovery mode for a broken SigmaOS install:
//! - recovery profiles (console, networking, graphical-fallback)
//! - a guided checklist of diagnostic/repair steps with pass/fail state
//! - snapshot & restore of user/system config files (dotfiles, fstab,
//!   network configs) to/from a recovery partition
//! - boot-entry inspection (via injectable backend)
//!
//! All host interaction goes through `RecoveryBackend` so tests are
//! deterministic; `HostBackend` performs real filesystem + bootloader ops.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Profile {
    /// Bare console: no network, no GUI — for filesystem repair.
    Console,
    /// Console + networking for package re-install / remote support.
    Network,
    /// Fallback graphical session with safe-mode compositor settings.
    GraphicalFallback,
}
impl Profile {
    pub fn as_str(&self) -> &'static str {
        match self { Self::Console=>"console", Self::Network=>"network", Self::GraphicalFallback=>"graphical-fallback" }
    }
    pub fn parse(s: &str) -> Result<Self, RecoveryError> {
        match s {
            "console" => Ok(Self::Console),
            "network" => Ok(Self::Network),
            "graphical-fallback" => Ok(Self::GraphicalFallback),
            other => Err(RecoveryError::BadProfile(other.into())),
        }
    }
    /// What the profile enables.
    pub fn capabilities(&self) -> &'static [&'static str] {
        match self {
            Self::Console => &["shell", "fsck", "mount"],
            Self::Network => &["shell", "fsck", "mount", "network", "pkg-install"],
            Self::GraphicalFallback => &["shell", "network", "gui-safe-mode"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StepStatus { Pending, Pass, Fail, Skip }

/// One guided recovery step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub id: String,
    pub title: String,
    /// Shell command or description of the check.
    pub action: String,
    /// Expected outcome description (for humans).
    pub expect: String,
    pub status: StepStatus,
    pub notes: String,
}

/// A snapshot of a config file stored in the recovery area.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConfigSnapshot {
    /// Original absolute path e.g. /etc/fstab.
    pub source: PathBuf,
    /// Path inside the recovery store (relative).
    pub stored_as: String,
    pub size: u64,
    pub sha256: String,
    pub created_unix: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryError {
    BadProfile(String),
    NoSuchStep(String),
    DuplicateStep(String),
    SnapshotFailed(String),
    RestoreFailed(String),
    Backend(String),
    NothingToRestore(String),
}
impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadProfile(p)=>write!(f,"bad profile {p}"),
            Self::NoSuchStep(id)=>write!(f,"no step {id}"),
            Self::DuplicateStep(id)=>write!(f,"step {id} exists"),
            Self::SnapshotFailed(e)=>write!(f,"snapshot: {e}"),
            Self::RestoreFailed(e)=>write!(f,"restore: {e}"),
            Self::Backend(e)=>write!(f,"backend: {e}"),
            Self::NothingToRestore(p)=>write!(f,"no snapshot for {p}"),
        }
    }
}
impl std::error::Error for RecoveryError {}

/// Host interaction abstraction.
pub trait RecoveryBackend {
    /// Read a file's bytes (for snapshot/restore).
    fn read_file(&self, path: &Path) -> Result<Vec<u8>, String>;
    /// Write bytes to path (create parent dirs).
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), String>;
    /// List files under a dir (relative paths), non-recursive-ok.
    fn list_dir(&self, dir: &Path) -> Result<Vec<PathBuf>, String>;
    /// Does path exist?
    fn exists(&self, path: &Path) -> bool;
    /// File size in bytes.
    fn size(&self, path: &Path) -> Result<u64, String>;
    /// SHA-256 hex of file contents.
    fn sha256_file(&self, path: &Path) -> Result<String, String>;
    /// Current unix time.
    fn now_unix(&self) -> u64;
    /// Boot entries known to the bootloader (names + paths).
    fn boot_entries(&self) -> Result<Vec<(String, String)>, String>;
    /// Set next-boot entry (None = default).
    fn set_next_boot(&self, entry: Option<&str>) -> Result<(), String>;
}

/// Real host backend.
pub struct HostBackend;
impl RecoveryBackend for HostBackend {
    fn read_file(&self, p: &Path) -> Result<Vec<u8>, String> { std::fs::read(p).map_err(|e| e.to_string()) }
    fn write_file(&self, p: &Path, d: &[u8]) -> Result<(), String> {
        if let Some(par) = p.parent() { std::fs::create_dir_all(par).map_err(|e| e.to_string())?; }
        std::fs::write(p, d).map_err(|e| e.to_string())
    }
    fn list_dir(&self, d: &Path) -> Result<Vec<PathBuf>, String> {
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() { out.push(e.path()); }
        }
        Ok(out)
    }
    fn exists(&self, p: &Path) -> bool { p.exists() }
    fn size(&self, p: &Path) -> Result<u64, String> {
        std::fs::metadata(p).map(|m| m.len()).map_err(|e| e.to_string())
    }
    fn sha256_file(&self, p: &Path) -> Result<String, String> {
        sha256_hex(&std::fs::read(p).map_err(|e| e.to_string())?)
    }
    fn now_unix(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }
    fn boot_entries(&self) -> Result<Vec<(String, String)>, String> {
        // Parse common boot entry dirs; return (name, path)
        let mut out = vec![];
        for dir in ["/boot/loader/entries", "/boot/efi/loader/entries", "/boot"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().map_or(false, |x| x=="conf") {
                        let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                        out.push((name, p.to_string_lossy().to_string()));
                    }
                }
            }
        }
        Ok(out)
    }
    fn set_next_boot(&self, _e: Option<&str>) -> Result<(), String> {
        // Requires bootctl/fwupd privileged ops; report unsupported honestly.
        Err("set_next_boot requires privileged bootloader integration".into())
    }
}

/// In-memory backend for tests.
pub struct MemBackend {
    pub files: std::cell::RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    pub time: std::cell::Cell<u64>,
    pub boot: Vec<(String, String)>,
    pub next_boot: std::cell::RefCell<Option<String>>,
}
impl MemBackend {
    pub fn new() -> Self {
        Self { files: std::cell::RefCell::new(BTreeMap::new()),
            time: std::cell::Cell::new(1_700_000_000),
            boot: vec![], next_boot: std::cell::RefCell::new(None) }
    }
    pub fn put(&self, path: &str, data: &[u8]) {
        self.files.borrow_mut().insert(PathBuf::from(path), data.to_vec());
    }
    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.borrow().get(Path::new(path)).cloned()
    }
}
impl RecoveryBackend for MemBackend {
    fn read_file(&self, p: &Path) -> Result<Vec<u8>, String> {
        self.files.borrow().get(p).cloned().ok_or_else(|| format!("no file {}", p.display()))
    }
    fn write_file(&self, p: &Path, d: &[u8]) -> Result<(), String> {
        self.files.borrow_mut().insert(p.to_path_buf(), d.to_vec()); Ok(())
    }
    fn list_dir(&self, d: &Path) -> Result<Vec<PathBuf>, String> {
        Ok(self.files.borrow().keys().filter(|k| k.starts_with(d)).cloned().collect())
    }
    fn exists(&self, p: &Path) -> bool { self.files.borrow().contains_key(p) }
    fn size(&self, p: &Path) -> Result<u64, String> {
        self.files.borrow().get(p).map(|v| v.len() as u64).ok_or_else(|| "missing".into())
    }
    fn sha256_file(&self, p: &Path) -> Result<String, String> {
        self.files.borrow().get(p).map(|v| sha256_hex(v).unwrap_or_default()).ok_or_else(|| "missing".into())
    }
    fn now_unix(&self) -> u64 { self.time.get() }
    fn boot_entries(&self) -> Result<Vec<(String, String)>, String> { Ok(self.boot.clone()) }
    fn set_next_boot(&self, e: Option<&str>) -> Result<(), String> {
        *self.next_boot.borrow_mut() = e.map(|s| s.to_string()); Ok(())
    }
}

fn sha256_hex(data: &[u8]) -> Result<String, String> {
    // Minimal SHA-256 (no external deps).
    const K: [u32; 64] = [
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
    let mut h: [u32;8] = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
    let bitlen = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4*i],chunk[4*i+1],chunk[4*i+2],chunk[4*i+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let (mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut hh) = (h[0],h[1],h[2],h[3],h[4],h[5],h[6],h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh=g; g=f; f=e; e=d.wrapping_add(t1); d=c; c=b; b=a; a=t1.wrapping_add(t2);
        }
        h[0]=h[0].wrapping_add(a); h[1]=h[1].wrapping_add(b); h[2]=h[2].wrapping_add(c); h[3]=h[3].wrapping_add(d);
        h[4]=h[4].wrapping_add(e); h[5]=h[5].wrapping_add(f); h[6]=h[6].wrapping_add(g); h[7]=h[7].wrapping_add(hh);
    }
    Ok(h.iter().map(|x| format!("{:08x}", x)).collect())
}

/// Recovery session state.
pub struct SigmaOSRecoveryTool {
    pub profile: Option<Profile>,
    pub steps: BTreeMap<String, Step>,
    pub snapshots: Vec<ConfigSnapshot>,
    /// Directory inside recovery store for snapshots.
    pub store_dir: PathBuf,
}

impl SigmaOSRecoveryTool {
    pub fn new(store_dir: PathBuf) -> Self {
        let mut m = Self { profile: None, steps: BTreeMap::new(), snapshots: vec![], store_dir };
        m.seed_steps();
        m
    }

    fn seed_steps(&mut self) {
        let seeds = [
            ("mount-root", "Mount root filesystem", "mount /dev/sdXN /mnt", "root fs mounted rw at /mnt", ),
            ("check-fstab", "Verify /etc/fstab entries", "findmnt --verify", "all fstab entries resolve", ),
            ("fsck", "Filesystem check", "fsck -f /dev/sdXN", "clean or repaired", ),
            ("network", "Bring up networking", "ip link set eth0 up && udhcpc", "interface has address", ),
            ("restore-configs", "Restore config snapshots", "sigmaos-recovery restore --all", "known-good configs back in place", ),
            ("reboot", "Reboot into normal system", "systemctl reboot", "boots default entry", ),
        ];
        for (id,title,action,expect) in seeds {
            self.steps.insert(id.into(), Step {
                id: id.into(), title: title.into(), action: action.into(),
                expect: expect.into(), status: StepStatus::Pending, notes: String::new(),
            });
        }
    }

    pub fn set_profile(&mut self, p: Profile) { self.profile = Some(p); }
    pub fn profile(&self) -> Option<Profile> { self.profile }

    pub fn add_step(&mut self, s: Step) -> Result<(), RecoveryError> {
        if self.steps.contains_key(&s.id) { return Err(RecoveryError::DuplicateStep(s.id.clone())); }
        self.steps.insert(s.id.clone(), s);
        Ok(())
    }

    pub fn step(&self, id: &str) -> Option<&Step> { self.steps.get(id) }
    pub fn steps(&self) -> impl Iterator<Item = &Step> { self.steps.values() }

    pub fn mark_step(&mut self, id: &str, status: StepStatus, notes: &str) -> Result<(), RecoveryError> {
        let s = self.steps.get_mut(id).ok_or_else(|| RecoveryError::NoSuchStep(id.into()))?;
        s.status = status;
        if !notes.is_empty() { s.notes = notes.into(); }
        Ok(())
    }

    /// Ordered checklist filtered by what the profile enables.
    pub fn checklist(&self) -> Vec<&Step> {
        // network/fsck steps only make sense for profiles that enable them
        let caps = self.profile.map(|p| p.capabilities()).unwrap_or(&["shell","fsck","mount"]);
        self.steps.values().filter(|s| {
            let needs_net = s.id == "network";
            let needs_fsck = s.id == "fsck" || s.id == "mount-root" || s.id == "check-fstab";
            (!needs_net || caps.contains(&"network")) && (!needs_fsck || caps.contains(&"fsck") || caps.contains(&"mount"))
        }).collect()
    }

    pub fn progress(&self) -> (usize, usize, usize) {
        let mut pass=0; let mut fail=0; let mut done=0;
        for s in self.steps.values() {
            match s.status {
                StepStatus::Pass => { pass+=1; done+=1; }
                StepStatus::Fail => { fail+=1; done+=1; }
                StepStatus::Skip => { done+=1; }
                StepStatus::Pending => {}
            }
        }
        (pass, fail, done)
    }

    /// Snapshot a config file into the recovery store.
    pub fn snapshot<B: RecoveryBackend>(&mut self, be: &B, source: &Path) -> Result<ConfigSnapshot, RecoveryError> {
        if !be.exists(source) {
            return Err(RecoveryError::SnapshotFailed(format!("{} missing", source.display())));
        }
        let data = be.read_file(source).map_err(|e| RecoveryError::SnapshotFailed(e))?;
        // stored name: flatten path, e.g. /etc/fstab -> etc_fstab_<ts>
        let flat: String = source.to_string_lossy().chars()
            .map(|c| if c=='/' {'_'} else {c}).collect::<String>()
            .trim_start_matches('_').to_string();
        let stored_as = format!("{}_{}", flat, be.now_unix());
        let dest = self.store_dir.join(&stored_as);
        be.write_file(&dest, &data).map_err(|e| RecoveryError::SnapshotFailed(e))?;
        let snap = ConfigSnapshot {
            source: source.to_path_buf(), stored_as,
            size: data.len() as u64,
            sha256: sha256_hex(&data).unwrap_or_default(),
            created_unix: be.now_unix(),
        };
        self.snapshots.push(snap.clone());
        Ok(snap)
    }

    /// Restore a snapshot back to its source path (verifies hash after write).
    pub fn restore<B: RecoveryBackend>(&self, be: &B, snap: &ConfigSnapshot) -> Result<(), RecoveryError> {
        let src = self.store_dir.join(&snap.stored_as);
        if !be.exists(&src) {
            return Err(RecoveryError::RestoreFailed(format!("snapshot {} missing", snap.stored_as)));
        }
        let data = be.read_file(&src).map_err(|e| RecoveryError::RestoreFailed(e))?;
        // integrity check
        let hash = sha256_hex(&data).unwrap_or_default();
        if hash != snap.sha256 {
            return Err(RecoveryError::RestoreFailed("snapshot hash mismatch".into()));
        }
        be.write_file(&snap.source, &data).map_err(|e| RecoveryError::RestoreFailed(e))?;
        // verify what landed
        let back = be.read_file(&snap.source).map_err(|e| RecoveryError::RestoreFailed(e))?;
        if sha256_hex(&back).unwrap_or_default() != snap.sha256 {
            return Err(RecoveryError::RestoreFailed("post-restore verify failed".into()));
        }
        Ok(())
    }

    /// Restore all snapshots (latest per source path wins).
    pub fn restore_all<B: RecoveryBackend>(&self, be: &B) -> Result<usize, RecoveryError> {
        let mut latest: BTreeMap<PathBuf, &ConfigSnapshot> = BTreeMap::new();
        for s in &self.snapshots {
            match latest.get(&s.source) {
                Some(prev) if prev.created_unix >= s.created_unix => {}
                _ => { latest.insert(s.source.clone(), s); }
            }
        }
        let mut n = 0;
        for (_, s) in latest {
            self.restore(be, s)?;
            n += 1;
        }
        Ok(n)
    }

    /// List snapshots, newest first.
    pub fn snapshots(&self) -> Vec<&ConfigSnapshot> {
        let mut v: Vec<&ConfigSnapshot> = self.snapshots.iter().collect();
        v.sort_by(|a,b| b.created_unix.cmp(&a.created_unix));
        v
    }

    /// Boot into a recovery profile: pick matching boot entry if present.
    pub fn enter_recovery<B: RecoveryBackend>(&self, be: &B) -> Result<String, RecoveryError> {
        let entries = be.boot_entries().map_err(RecoveryError::Backend)?;
        let want = self.profile.map(|p| p.as_str()).unwrap_or("console");
        // look for entry whose name contains profile or "recovery"
        for (name, path) in &entries {
            let n = name.to_lowercase();
            if n.contains(want) || n.contains("recovery") {
                be.set_next_boot(Some(name)).map_err(RecoveryError::Backend)?;
                return Ok(format!("next boot -> {} ({})", name, path));
            }
        }
        Err(RecoveryError::Backend(format!("no boot entry matching profile '{want}'")))
    }

    pub fn status_line(&self) -> String {
        let (p,f,d) = self.progress();
        format!("recovery: profile={} steps={}/{} pass={} fail={} snaps={}",
            self.profile.map(|p| p.as_str()).unwrap_or("none"), d, self.steps.len(), p, f, self.snapshots.len())
    }
}

// ---- persistence ----
#[derive(serde::Serialize, serde::Deserialize)]
struct Persist {
    profile: Option<Profile>,
    steps: Vec<Step>,
    snapshots: Vec<ConfigSnapshot>,
}
impl SigmaOSRecoveryTool {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let p = Persist { profile: self.profile, steps: self.steps.values().cloned().collect(),
            snapshots: self.snapshots.clone() };
        let j = serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?;
        if let Some(d) = path.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
        std::fs::write(path, j).map_err(|e| e.to_string())
    }
    pub fn load(path: &Path, store_dir: PathBuf) -> Result<Self, String> {
        let txt = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let p: Persist = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
        let mut m = Self::new(store_dir);
        m.profile = p.profile;
        m.steps.clear();
        for s in p.steps { m.steps.insert(s.id.clone(), s); }
        m.snapshots = p.snapshots;
        Ok(m)
    }
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("SIGMAOS_RECOVERY_FILE") { return PathBuf::from(p); }
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
            .join(".local/share/sigmaos/recovery.json")
    }
    pub fn default_store() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
            .join(".local/share/sigmaos/recovery-store")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_parse_and_caps() {
        assert_eq!(Profile::parse("console").unwrap(), Profile::Console);
        assert_eq!(Profile::parse("network").unwrap().capabilities().contains(&"network"), true);
        assert_eq!(Profile::Console.capabilities().contains(&"network"), false);
        assert!(Profile::parse("bogus").is_err());
    }

    #[test]
    fn step_lifecycle() {
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/tmp/rec"));
        assert_eq!(m.steps.len(), 6);
        m.mark_step("mount-root", StepStatus::Pass, "mounted").unwrap();
        m.mark_step("fsck", StepStatus::Fail, "errors remain").unwrap();
        assert!(m.mark_step("nope", StepStatus::Pass, "").is_err());
        let (p,f,d) = m.progress();
        assert_eq!((p,f,d), (1,1,2));
        // duplicate step rejected
        let s = Step { id:"mount-root".into(), title:"x".into(), action:"y".into(),
            expect:"z".into(), status:StepStatus::Pending, notes:String::new() };
        assert!(m.add_step(s).is_err());
    }

    #[test]
    fn checklist_filtered_by_profile() {
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/tmp/rec"));
        // console profile: no network step
        m.set_profile(Profile::Console);
        let cl = m.checklist();
        assert!(!cl.iter().any(|s| s.id == "network"));
        assert!(cl.iter().any(|s| s.id == "mount-root"));
        // network profile includes network step
        m.set_profile(Profile::Network);
        let cl2 = m.checklist();
        assert!(cl2.iter().any(|s| s.id == "network"));
    }

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let be = MemBackend::new();
        be.put("/etc/fstab", b"UUID=abc / ext4 defaults 0 1\n");
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
        let snap = m.snapshot(&be, Path::new("/etc/fstab")).unwrap();
        assert_eq!(snap.size, 29);
        assert_eq!(snap.sha256.len(), 64);
        // stored file exists in backend at store_dir/stored_as
        let stored = PathBuf::from("/rec/store").join(&snap.stored_as);
        assert!(be.exists(&stored));
        // corrupt the source, restore
        be.put("/etc/fstab", b"corrupted\n");
        m.restore(&be, &snap).unwrap();
        assert_eq!(be.get("/etc/fstab").unwrap(), b"UUID=abc / ext4 defaults 0 1\n".to_vec());
    }

    #[test]
    fn restore_detects_tampering() {
        let be = MemBackend::new();
        be.put("/etc/hosts", b"127.0.0.1 localhost\n");
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
        let snap = m.snapshot(&be, Path::new("/etc/hosts")).unwrap();
        // tamper with stored snapshot
        let stored = PathBuf::from("/rec/store").join(&snap.stored_as);
        be.put(stored.to_str().unwrap(), b"evil\n");
        assert!(m.restore(&be, &snap).is_err());
    }

    #[test]
    fn restore_all_latest_wins() {
        let be = MemBackend::new();
        be.put("/etc/fstab", b"v1\n");
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
        m.snapshot(&be, Path::new("/etc/fstab")).unwrap();
        be.time.set(1_700_000_100);
        be.put("/etc/fstab", b"v2-newer\n");
        m.snapshot(&be, Path::new("/etc/fstab")).unwrap();
        assert_eq!(m.snapshots.len(), 2);
        be.put("/etc/fstab", b"corrupt\n");
        let n = m.restore_all(&be).unwrap();
        assert_eq!(n, 1);
        assert_eq!(be.get("/etc/fstab").unwrap(), b"v2-newer\n".to_vec());
    }

    #[test]
    fn snapshot_missing_source_fails() {
        let be = MemBackend::new();
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
        assert!(m.snapshot(&be, Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn enter_recovery_picks_entry() {
        let mut be = MemBackend::new();
        be.boot = vec![
            ("sigmaos".into(), "/boot/sigmaos.conf".into()),
            ("sigmaos-recovery-console".into(), "/boot/rec.conf".into()),
        ];
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
        m.set_profile(Profile::Console);
        let r = m.enter_recovery(&be).unwrap();
        assert!(r.contains("sigmaos-recovery-console"));
        assert_eq!(be.next_boot.borrow().as_deref(), Some("sigmaos-recovery-console"));
        // no matching entry → error
        be.boot = vec![("other".into(), "/x".into())];
        assert!(m.enter_recovery(&be).is_err());
    }

    #[test]
    fn sha256_known_vectors() {
        // SHA-256("") = e3b0c442...
        assert_eq!(sha256_hex(b"").unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        // SHA-256("abc") = ba7816bf...
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sigmaos-rec-test-{}", std::process::id()));
        let p = dir.join("rec.json");
        let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
        m.set_profile(Profile::Network);
        m.mark_step("network", StepStatus::Pass, "up").unwrap();
        m.save(&p).unwrap();
        let m2 = SigmaOSRecoveryTool::load(&p, PathBuf::from("/rec/store")).unwrap();
        assert_eq!(m2.profile, Some(Profile::Network));
        assert_eq!(m2.step("network").unwrap().status, StepStatus::Pass);
        assert_eq!(m2.steps.len(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
