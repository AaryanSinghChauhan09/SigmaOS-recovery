//! SigmaOS-recovery — recovery CLI.
//!
//! Usage:
//!   sigmaos-recovery status | profile <console|network|graphical-fallback>
//!   sigmaos-recovery checklist | step <id> pass|fail|skip [notes]
//!   sigmaos-recovery snapshot <path>... | snapshots | restore <stored_as|source> | restore-all
//!   sigmaos-recovery boot-entries | enter <profile>
//!
//! Snapshots land in ~/.local/share/sigmaos/recovery-store/;
//! state in ~/.local/share/sigmaos/recovery.json

use recovery::{HostBackend, SigmaOSRecoveryTool, Profile, RecoveryBackend, StepStatus};
use std::path::{Path, PathBuf};
use std::process::exit;

fn usage() -> ! {
    eprintln!("usage: sigmaos-recovery <status|profile|checklist|step|snapshot|snapshots|restore|restore-all|boot-entries|enter> [args]");
    exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() { usage(); }
    let db = SigmaOSRecoveryTool::default_path();
    let store = SigmaOSRecoveryTool::default_store();
    let mut m = SigmaOSRecoveryTool::load(&db, store.clone()).unwrap_or_else(|_| SigmaOSRecoveryTool::new(store.clone()));
    let be = HostBackend;
    let cmd = args[0].as_str();
    let rest = &args[1..];
    let arg = |i: usize| -> &str { rest.get(i).map(|s| s.as_str()).unwrap_or_else(|| usage()) };

    let result: Result<(), String> = (|| -> Result<(), String> { match cmd {
        "status" => { println!("{}", m.status_line()); Ok(()) }
        "profile" => {
            let p = Profile::parse(arg(0)).map_err(|e| e.to_string())?;
            m.set_profile(p);
            println!("profile -> {} (caps: {})", p.as_str(), p.capabilities().join(","));
            Ok(())
        }
        "checklist" => {
            for s in m.checklist() {
                let st = match s.status { StepStatus::Pending=>"...", StepStatus::Pass=>"PASS", StepStatus::Fail=>"FAIL", StepStatus::Skip=>"SKIP" };
                println!("[{}] {:<18} {} -> {}", st, s.id, s.action, s.expect);
            }
            Ok(())
        }
        "step" => {
            let id=arg(0);
            let st = match arg(1) { "pass"=>StepStatus::Pass, "fail"=>StepStatus::Fail, "skip"=>StepStatus::Skip, _=>usage() };
            let notes = rest.get(2).cloned().unwrap_or_default();
            m.mark_step(id, st, &notes).map_err(|e| e.to_string())?;
            println!("{} -> {:?}", id, st); Ok(())
        }
        "snapshot" => {
            for p in rest {
                let s = m.snapshot(&be, Path::new(p)).map_err(|e| e.to_string())?;
                println!("snapshotted {} -> {} ({} bytes, sha256 {}…)", s.source.display(), s.stored_as, s.size, &s.sha256[..12]);
            }
            Ok(())
        }
        "snapshots" => {
            for s in m.snapshots() {
                println!("{}  {:<10}B  {} -> {}", s.created_unix, s.size, s.stored_as, s.source.display());
            }
            if m.snapshots.is_empty() { println!("(no snapshots)"); }
            Ok(())
        }
        "restore" => {
            let q = arg(0);
            // find by stored_as or source
            let snap = m.snapshots.iter().find(|s| s.stored_as==q || s.source.to_string_lossy()==q)
                .ok_or_else(|| format!("no snapshot matching {q}"))?;
            m.restore(&be, snap).map_err(|e| e.to_string())?;
            println!("restored {} -> {}", snap.stored_as, snap.source.display()); Ok(())
        }
        "restore-all" => {
            let n = m.restore_all(&be).map_err(|e| e.to_string())?;
            println!("restored {} config(s)", n); Ok(())
        }
        "boot-entries" => {
            let es = be.boot_entries().map_err(|e| e.to_string())?;
            let empty = es.is_empty();
            for (n,p) in &es { println!("{}  {}", n, p); }
            if empty { println!("(none found)"); }
            Ok(())
        }
        "enter" => {
            let p = Profile::parse(arg(0)).map_err(|e| e.to_string())?;
            m.set_profile(p);
            println!("{}", m.enter_recovery(&be).map_err(|e| e.to_string())?); Ok(())
        }
        _ => usage(),
    } })();

    match result {
        Ok(()) => { if let Err(e)=m.save(&db) { eprintln!("sigmaos-recovery: warn: save: {e}"); } }
        Err(e) => { eprintln!("sigmaos-recovery: {e}"); exit(1); }
    }
}
