//! The background service against the machine's real systemd user manager — opt-in, because
//! it writes a unit into the real unit directory (under its own name, removed at the end):
//!
//!   cargo test --test live_daemon -- --ignored --nocapture
//!
//! Proves what the scripted tests cannot: the unit parses and starts, systemd restarts a
//! killed `posd`, a stop lets a sale in flight finish, and removal leaves nothing behind.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use payment_pos_app_lib::daemon::{self, real, Host, Mode, Target, Timeouts};

fn systemctl(args: &[&str]) -> String {
    let out = Command::new("systemctl").arg("--user").args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn main_pid(unit: &str) -> u32 {
    systemctl(&["show", "-p", "MainPID", "--value", unit]).parse().unwrap_or(0)
}

fn wait_for(what: &str, cap: Duration, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(started.elapsed() < cap, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "touches the real systemd user manager"]
fn the_service_round_trip() {
    let root = std::env::temp_dir().join(format!("ledger-live-{}", std::process::id()));
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let unit = format!("ledger-pos-live-{}.service", std::process::id());
    // Edition 2021: set_var is safe; nothing else runs in this test binary.
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_DATA_HOME", root.join("data"));
    std::env::set_var("LEDGER_POS_DAEMON_UNIT", &unit);
    std::env::set_var("LEDGER_POS_LINGER", "0");
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    let settings = root.join("config/ledger-pos/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        format!(
            r#"{{ "server": {{ "host": "127.0.0.1", "port": {port} }},
                 "providers": {{ "active": "sandbox", "configs": {{ "sandbox": {{ "latencyMs": 0, "cardPresentDelayMs": 6000 }} }} }} }}"#
        ),
    )
    .unwrap();

    let host = real::RealHost::new();
    let paths = real::paths();
    assert_eq!(paths.unit_name, unit);
    let target = Target { exe: PathBuf::from(env!("CARGO_BIN_EXE_posd")), mode: Mode::Service };

    // 1. Installed, started, answering — as posd, of the binary's own version.
    let status = daemon::ensure(&host, &paths, &target, &Timeouts::default());
    println!("ensure: {status:?}");
    assert_eq!((status.mode, status.state), ("service", "serving"), "{status:?}");
    let health = host.health().unwrap();
    assert_eq!(health.process.as_deref(), Some("posd"));
    assert_eq!(systemctl(&["is-enabled", &unit]), "enabled");

    // 2. A second ensure changes nothing and keeps the same process.
    let pid = main_pid(&unit);
    assert!(pid > 0);
    assert_eq!(daemon::ensure(&host, &paths, &target, &Timeouts::default()).state, "serving");
    assert_eq!(main_pid(&unit), pid, "an unchanged, healthy service is not restarted");

    // 2b. posd publishes where it serves; a configured port it is not (yet) on — a failed
    // rebind keeps the last good one — is no reason to restart it and lose that address.
    let serving = root.join("config/ledger-pos/serving.json");
    let published = payment_pos_app_lib::server::read_serving(&serving).expect("serving.json");
    assert_eq!((published.pid, published.port), (pid, port));
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let text = std::fs::read_to_string(&settings).unwrap().replace(&port.to_string(), &other.to_string());
    std::fs::write(&settings, text).unwrap();
    assert_eq!(real::api_endpoint().1, port, "the live posd's address wins over the file");
    assert_eq!(daemon::ensure(&host, &paths, &target, &Timeouts::default()).state, "serving");
    assert_eq!(main_pid(&unit), pid, "not restarted over a port it has not moved to");
    let text = std::fs::read_to_string(&settings).unwrap().replace(&other.to_string(), &port.to_string());
    std::fs::write(&settings, text).unwrap();

    // 3. Killed outright, systemd brings it back.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    wait_for("systemd to restart posd", Duration::from_secs(20), || {
        let now = main_pid(&unit);
        now != 0 && now != pid && host.health().is_some()
    });
    println!("restarted: pid {pid} -> {}", main_pid(&unit));

    // 4. A stop waits for the sale in flight.
    let sale = std::thread::spawn(move || {
        Command::new("curl")
            .args(["-sS", "-m", "60", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST"])
            .args(["-H", "content-type: application/json", "-d", r#"{"mainAmount":1000}"#])
            .arg(format!("http://127.0.0.1:{port}/api/v1/providers/sandbox/functions/purchase/invoke"))
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap()
    });
    std::thread::sleep(Duration::from_millis(1500));
    let stop_started = Instant::now();
    systemctl(&["stop", &unit]);
    let code = sale.join().unwrap();
    println!("sale during stop: HTTP {code}; stop took {:?}", stop_started.elapsed());
    assert_eq!(code, "200", "the sale in flight was cut off");
    assert!(stop_started.elapsed() >= Duration::from_secs(3), "the stop did not wait for the sale");

    // 5. Removal leaves no unit, no process, no answer.
    daemon::ensure(&host, &paths, &target, &Timeouts::default());
    daemon::remove(&host, &paths, &target.exe);
    let unit_file = host.manager_unit_dir().unwrap().join(&unit);
    assert!(!unit_file.exists(), "{} left behind", unit_file.display());
    wait_for("the API to go down", Duration::from_secs(10), || host.health().is_none());
    let _ = std::fs::remove_dir_all(&root);
}
