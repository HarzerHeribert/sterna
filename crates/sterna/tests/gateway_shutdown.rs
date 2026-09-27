//! A gateway that does not leave when its stdin closes is ended with every
//! process it started: a subscription sidecar must never outlive the session
//! that started it.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

#[test]
fn a_gateway_that_will_not_stop_takes_its_children_down_with_it() {
    let dir = std::env::temp_dir().join(format!("sterna-gw-shutdown-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("child.pid");
    let script = dir.join("inference-gateway");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep 300 &\necho $! > '{}'\necho '{{\"listening\":\"http://127.0.0.1:9\"}}'\nexec sleep 300\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let serving = sterna::gateway::Gateway::Command { gateway: script }
        .serve(&dir.join("gateway.log"))
        .expect("the fake gateway announces itself");
    let child: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only asks whether the process exists.
    assert_eq!(
        unsafe { libc::kill(child, 0) },
        0,
        "the sidecar stand-in is running"
    );

    drop(serving);

    std::thread::sleep(std::time::Duration::from_millis(200));
    // SAFETY: as above.
    let alive = unsafe { libc::kill(child, 0) } == 0;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!alive, "the gateway's child outlived it");
}
