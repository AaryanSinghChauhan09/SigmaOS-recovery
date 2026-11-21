// SPDX-License-Identifier: Apache-2.0
use recovery::*;
use std::path::{Path, PathBuf};

#[test]
fn end_to_end_recovery_workflow() {
    let be = MemBackend::new();
    be.put("/etc/fstab", b"UUID=x / ext4 defaults 0 1\n");
    let mut m = SigmaOSRecoveryTool::new(PathBuf::from("/rec/store"));
    m.set_profile(Profile::Network);
    m.snapshot(&be, Path::new("/etc/fstab")).unwrap();
    // simulate breakage + guided repair
    be.put("/etc/fstab", b"garbage\n");
    m.mark_step("check-fstab", StepStatus::Fail, "bad entry").unwrap();
    m.mark_step("restore-configs", StepStatus::Pass, "restored").unwrap();
    let n = m.restore_all(&be).unwrap();
    assert_eq!(n, 1);
    assert_eq!(be.get("/etc/fstab").unwrap(), b"UUID=x / ext4 defaults 0 1\n".to_vec());
    let (p,f,d) = m.progress();
    assert_eq!((p,f,d),(1,1,2));
}
