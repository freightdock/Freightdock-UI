//! The server's log is in local time: the zone `TZ` names, else the machine's (the container's `/etc/localtime`), with
//! the offset written, so a line can always be read back to UTC. The real binary is started, as the image starts it,
//! with a Docker that does not answer (a warning, logged) and plain HTTP on a port of its own; its first line is read.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// The first line the server logs, started in zone `tz`.
fn first_line(tz: &str) -> String {
    let data = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_freightdock"))
        .env("TZ", tz)
        .env("FREIGHTDOCK_DATA_DIR", data.path())
        .env("FREIGHTDOCK_BIND", "127.0.0.1:0")
        .env("FREIGHTDOCK_TLS", "off")
        .env("FREIGHTDOCK_DOCKER_HOST", "tcp://127.0.0.1:1")
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let _ = child.kill();
    let _ = child.wait();
    line
}

#[test]
fn log_lines_are_in_the_zone_tz_names_with_its_offset() {
    // India keeps no summer time, so its offset is the same whatever the date the test runs on.
    let line = first_line("Asia/Kolkata");
    let stamp = line.split_whitespace().next().unwrap_or_default();
    assert!(stamp.ends_with("+05:30"), "a local time with its offset first: {line:?}");
    let line = first_line("UTC");
    let stamp = line.split_whitespace().next().unwrap_or_default();
    assert!(stamp.ends_with("+00:00"), "UTC is a zone like any other: {line:?}");
}
