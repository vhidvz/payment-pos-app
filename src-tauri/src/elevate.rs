//! Administrator actions from the console.
//!
//! The password, auto-lock, webhook subscriptions and the receipt key live in the
//! administrator file, which only `posd admin` run as root writes — so an operator who
//! knows the console password still cannot switch the monitoring off. The console keeps
//! that: it runs the very same `posd admin` through **pkexec**, and the operating system
//! asks for the administrator's password in its own dialog. Nothing here is a new way in;
//! it is `sudo posd admin …` with a window instead of a terminal.
//!
//! Only the operations below can be run — never an argument list from the page. Where
//! the OS cannot ask (no pkexec, no polkit agent, Windows, a `posd` root cannot reach),
//! the console shows the terminal command instead.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A password that never shows up in a log or a debug print.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// One administrator operation, as the console asks for it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum AdminOp {
    SetPassword { password: Secret },
    ClearPassword,
    AutoLock { minutes: u32 },
    EventsAdd { url: String, events: Vec<String> },
    EventsRemove { id: String },
    EventsEnable { id: String, enabled: bool },
    EventsTest { id: Option<String> },
    ReceiptKeySet { kid: Option<String> },
    ReceiptKeyClear,
}

/// What became of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AdminRunResult {
    /// `posd` ran and answered this JSON (secrets included, shown once by the console).
    Done { output: Value },
    /// The administrator dismissed the system's password dialog. Nothing changed.
    Cancelled,
    /// The console cannot ask the OS here; run `command` in a terminal instead.
    Manual { command: String, reason: String },
    /// `posd` refused or failed; `message` is its own explanation.
    Failed { message: String, command: String },
}

/// Before running: can the console ask the OS itself, and what is the terminal form?
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPreview {
    pub in_app: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The arguments after `posd`: always `admin --json …`; the password goes on stdin.
pub fn admin_args(op: &AdminOp) -> Vec<String> {
    let mut a: Vec<String> = vec!["admin".into(), "--json".into()];
    a.extend(command_words(op, true));
    a
}

/// The words after `admin`. `piped`: the console's form (`set-password --stdin`).
fn command_words(op: &AdminOp, piped: bool) -> Vec<String> {
    let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match op {
        AdminOp::SetPassword { .. } if piped => w(&["set-password", "--stdin"]),
        AdminOp::SetPassword { .. } => w(&["set-password"]),
        AdminOp::ClearPassword => w(&["clear-password"]),
        AdminOp::AutoLock { minutes } => vec!["auto-lock".into(), minutes.to_string()],
        AdminOp::EventsAdd { url, events } => {
            let mut v = vec!["events".into(), "add".into(), url.clone()];
            // `*` is the default; leaving it out keeps the command what a person would type.
            if !events.is_empty() && events.iter().any(|e| e != "*") {
                v.push("--events".into());
                v.push(events.join(","));
            }
            v
        }
        AdminOp::EventsRemove { id } => vec!["events".into(), "remove".into(), id.clone()],
        AdminOp::EventsEnable { id, enabled } => {
            vec!["events".into(), (if *enabled { "enable" } else { "disable" }).into(), id.clone()]
        }
        AdminOp::EventsTest { id } => {
            let mut v = w(&["events", "test"]);
            v.extend(id.clone());
            v
        }
        AdminOp::ReceiptKeySet { kid } => {
            let mut v = w(&["receipt-key", "set"]);
            if let Some(kid) = kid {
                v.push("--kid".into());
                v.push(kid.clone());
            }
            v
        }
        AdminOp::ReceiptKeyClear => w(&["receipt-key", "clear"]),
    }
}

/// What `posd` reads on stdin for `op`, if anything.
pub fn stdin_of(op: &AdminOp) -> Option<String> {
    match op {
        AdminOp::SetPassword { password } => Some(format!("{}\n", password.0)),
        _ => None,
    }
}

/// Quote one word for a POSIX shell, or for cmd.exe on Windows.
fn quote(word: &str, windows: bool) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:,=@%+-".contains(c) || (windows && c == '\\'));
    match (plain, windows) {
        (true, _) => word.to_string(),
        (false, true) => format!("\"{}\"", word.replace('"', "\"\"")),
        (false, false) => format!("'{}'", word.replace('\'', r"'\''")),
    }
}

/// The same operation as a person types it: `sudo` (not on Windows), no `--json`, the
/// password prompted for rather than piped, every argument quoted for the shell.
pub fn terminal_command(op: &AdminOp, posd: &Path, windows: bool) -> String {
    let mut parts = Vec::new();
    if !windows {
        parts.push("sudo".to_string());
    }
    parts.push(quote(&posd.to_string_lossy(), windows));
    parts.push("admin".into());
    parts.extend(command_words(op, false).iter().map(|w| quote(w, windows)));
    parts.join(" ")
}

/// Map pkexec's exit to an outcome. pkexec answers 126 when the dialog was dismissed and
/// 127 when it could not ask at all (no authentication agent, or not authorised).
pub fn classify(code: Option<i32>, stdout: &str, stderr: &str, command: &str) -> AdminRunResult {
    let said = stderr.trim();
    match code {
        Some(0) => match stdout.lines().rev().find(|l| !l.trim().is_empty()).map(serde_json::from_str::<Value>) {
            Some(Ok(output)) => AdminRunResult::Done { output },
            _ => AdminRunResult::Failed {
                message: "posd finished but did not answer in a form the console reads — it may be \
                          an older version. Run the command in a terminal instead."
                    .into(),
                command: command.into(),
            },
        },
        Some(126) => AdminRunResult::Cancelled,
        // 127 is also "Not authorized" — the password was refused, or policy forbids it.
        Some(127) if said.contains("Not authorized") => AdminRunResult::Failed {
            message: "The administrator password was not accepted.".into(),
            command: command.into(),
        },
        Some(127) => AdminRunResult::Manual {
            command: command.into(),
            reason: if said.is_empty() {
                "The system could not ask for the administrator password here.".into()
            } else {
                format!("The system could not ask for the administrator password here ({said}).")
            },
        },
        Some(n) => AdminRunResult::Failed {
            message: if said.is_empty() { format!("posd stopped with exit code {n}") } else { said.into() },
            command: command.into(),
        },
        None => AdminRunResult::Failed {
            message: "posd was stopped before it finished".into(),
            command: command.into(),
        },
    }
}

/// The `posd` to run as root. From an AppImage that is the installed copy: the running
/// file is mounted through FUSE, which root cannot read.
pub fn posd_for_elevation(exe: &Path, appimage: Option<&Path>, installed_posd: &Path) -> Result<PathBuf, String> {
    if appimage.is_some() {
        return if installed_posd.is_file() {
            Ok(installed_posd.to_path_buf())
        } else {
            Err("Add Ledger POS to your applications menu first (Settings → Desktop integration): \
                 the administrator cannot reach a program inside a running AppImage."
                .into())
        };
    }
    let name = if cfg!(windows) { "posd.exe" } else { "posd" };
    let beside = exe.parent().map(|d| d.join(name)).filter(|p| p.is_file());
    beside.ok_or_else(|| format!("posd was not found next to the app ({}).", exe.display()))
}

/// `Ok` when only root can change `path`: the file and every directory above it are
/// root-owned and writable by no group or other account. pkexec runs what it is given as
/// root, so the binary has to be one the desktop user cannot swap.
#[cfg(unix)]
pub fn root_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    for p in real.ancestors() {
        let m = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err(format!("{} can be changed without the administrator", p.display()));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn root_only(path: &Path) -> Result<(), String> {
    Err(format!("{}: ownership cannot be checked on this system", path.display()))
}

/// The system's own pkexec — never one found on `PATH`, which can start in a directory
/// the user owns: a fake would show a fake dialog and collect the password.
#[cfg(unix)]
fn trusted_pkexec() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    ["/usr/bin/pkexec", "/bin/pkexec"].iter().map(PathBuf::from).find(|p| {
        root_only(p).is_ok() && std::fs::metadata(p).is_ok_and(|m| m.mode() & 0o4000 != 0)
    })
}

#[cfg(not(unix))]
fn trusted_pkexec() -> Option<PathBuf> {
    None
}

/// What `posd --version` says, run as this user.
fn posd_version(posd: &Path) -> Option<String> {
    let out = std::process::Command::new(posd).arg("--version").output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().strip_prefix("posd ").map(str::to_string)
}

/// The `posd` to hand to pkexec: `found`, or a root-owned copy in `/usr/local/bin` or
/// `/usr/bin` — the first one only root can change **and** of this console's version
/// (an older one would not understand `--json`). Otherwise why not, and the one command
/// that fixes it.
pub fn elevation_target(
    found: &Path,
    exists: &dyn Fn(&Path) -> bool,
    trusted: &dyn Fn(&Path) -> Result<(), String>,
    version_of: &dyn Fn(&Path) -> Option<String>,
    want: &str,
) -> Result<PathBuf, String> {
    let mut candidates = vec![found.to_path_buf()];
    for c in ["/usr/local/bin/posd", "/usr/bin/posd"].map(PathBuf::from) {
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    }
    let refresh = format!("sudo install -o root -g root -m 755 {} /usr/local/bin/posd", quote(&found.to_string_lossy(), false));
    let mut stale = None;
    for c in candidates.iter().filter(|c| exists(c)) {
        if trusted(c).is_err() {
            continue;
        }
        match version_of(c) {
            Some(v) if v == want => return Ok(c.clone()),
            v => stale = stale.or(Some((c.clone(), v.unwrap_or_else(|| "unknown".into())))),
        }
    }
    Err(match stale {
        Some((p, v)) if p == found => format!(
            "posd at {} is version {v}, this console is {want}; install matching versions of both.",
            p.display()
        ),
        Some((p, v)) => format!(
            "The root-owned posd at {} is version {v}, but this console is {want}. Refresh it once, in a terminal:\n\n    {refresh}",
            p.display()
        ),
        None => format!(
            "{} can be changed without the administrator's password, so Ledger POS will not ask the \
             administrator to run it. Once, in a terminal, put a copy only root can change where the \
             console looks for it:\n\n    {refresh}",
            found.display()
        ),
    })
}

/// Where this process would find `posd`, and the path to show in a terminal command.
fn locate() -> (Result<PathBuf, String>, PathBuf) {
    let installed = crate::install::Layout::current().posd_path();
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let found = std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| posd_for_elevation(&exe, appimage.as_deref(), &installed));
    let shown = match (&found, appimage.is_some()) {
        (Ok(p), _) => p.clone(),
        (Err(_), true) => installed,
        (Err(_), false) => PathBuf::from("posd"),
    };
    (found, shown)
}

/// Everything `preview` and `run` decide alike: what pkexec would run — or why the console
/// will not ask here — and the terminal form of `op`.
fn plan(op: &AdminOp) -> (Result<(PathBuf, PathBuf), String>, String) {
    let (found, shown) = locate();
    let target = found.and_then(|p| {
        elevation_target(&p, &|c: &Path| c.is_file(), &root_only, &posd_version, crate::version::VERSION)
    });
    let command = terminal_command(op, target.as_deref().unwrap_or(&shown), cfg!(windows));
    let planned = target.and_then(|posd| {
        if !cfg!(target_os = "linux") {
            return Err("Run it from an administrator command prompt.".into());
        }
        trusted_pkexec()
            .map(|pkexec| (pkexec, posd))
            .ok_or_else(|| "This system has no pkexec to ask for the administrator password.".into())
    });
    (planned, command)
}

/// Whether the console can ask the OS itself, and the terminal form either way.
pub fn preview(op: &AdminOp) -> AdminPreview {
    let (planned, command) = plan(op);
    let reason = planned.err();
    AdminPreview { in_app: reason.is_none(), command, reason }
}

/// Run `op` as the administrator: the OS asks for the password, `posd` does the work.
/// Blocks until the dialog is answered — call it off the async runtime.
pub fn run(op: &AdminOp) -> AdminRunResult {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let (planned, command) = plan(op);
    let (pkexec, posd) = match planned {
        Ok(p) => p,
        Err(reason) => return AdminRunResult::Manual { command, reason },
    };
    let child = Command::new(pkexec)
        .arg(&posd)
        .args(admin_args(op))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return AdminRunResult::Manual { command, reason: format!("pkexec could not start: {e}") },
    };
    if let (Some(input), Some(mut pipe)) = (stdin_of(op), child.stdin.take()) {
        // posd reads one line; a closed pipe here means it already failed and says why.
        let _ = pipe.write_all(input.as_bytes());
    }
    drop(child.stdin.take());
    match child.wait_with_output() {
        Ok(out) => classify(
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
            &command,
        ),
        Err(e) => AdminRunResult::Failed { message: e.to_string(), command },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(v: Value) -> AdminOp {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn ops_arrive_from_the_page_as_tagged_objects() {
        assert_eq!(op(json!({"op": "autoLock", "minutes": 15})), AdminOp::AutoLock { minutes: 15 });
        assert_eq!(
            op(json!({"op": "eventsEnable", "id": "sub_1", "enabled": false})),
            AdminOp::EventsEnable { id: "sub_1".into(), enabled: false }
        );
        assert!(serde_json::from_value::<AdminOp>(json!({"op": "rm", "path": "/"})).is_err(), "no free-form op");
        let set = op(json!({"op": "setPassword", "password": "hunter22"}));
        assert!(!format!("{set:?}").contains("hunter22"), "a password never reaches a debug print");
    }

    #[test]
    fn each_op_becomes_its_posd_admin_command() {
        let a = |v| admin_args(&op(v));
        assert_eq!(a(json!({"op": "setPassword", "password": "p4ss"})), ["admin", "--json", "set-password", "--stdin"]);
        assert_eq!(a(json!({"op": "clearPassword"})), ["admin", "--json", "clear-password"]);
        assert_eq!(a(json!({"op": "autoLock", "minutes": 0})), ["admin", "--json", "auto-lock", "0"]);
        assert_eq!(a(json!({"op": "eventsAdd", "url": "https://h/x", "events": ["*"]})), ["admin", "--json", "events", "add", "https://h/x"]);
        assert_eq!(a(json!({"op": "eventsAdd", "url": "https://h/x", "events": []})), ["admin", "--json", "events", "add", "https://h/x"]);
        assert_eq!(
            a(json!({"op": "eventsAdd", "url": "https://h/x", "events": ["security.*", "settings.changed"]})),
            ["admin", "--json", "events", "add", "https://h/x", "--events", "security.*,settings.changed"]
        );
        assert_eq!(a(json!({"op": "eventsRemove", "id": "sub_1"})), ["admin", "--json", "events", "remove", "sub_1"]);
        assert_eq!(a(json!({"op": "eventsEnable", "id": "sub_1", "enabled": true})), ["admin", "--json", "events", "enable", "sub_1"]);
        assert_eq!(a(json!({"op": "eventsEnable", "id": "sub_1", "enabled": false})), ["admin", "--json", "events", "disable", "sub_1"]);
        assert_eq!(a(json!({"op": "eventsTest", "id": null})), ["admin", "--json", "events", "test"]);
        assert_eq!(a(json!({"op": "eventsTest", "id": "sub_1"})), ["admin", "--json", "events", "test", "sub_1"]);
        assert_eq!(a(json!({"op": "receiptKeySet", "kid": null})), ["admin", "--json", "receipt-key", "set"]);
        assert_eq!(a(json!({"op": "receiptKeySet", "kid": "shop-7"})), ["admin", "--json", "receipt-key", "set", "--kid", "shop-7"]);
        assert_eq!(a(json!({"op": "receiptKeyClear"})), ["admin", "--json", "receipt-key", "clear"]);
    }

    #[test]
    fn only_the_password_travels_on_stdin_never_in_the_arguments() {
        let set = op(json!({"op": "setPassword", "password": "p4ss word"}));
        assert_eq!(stdin_of(&set).as_deref(), Some("p4ss word\n"));
        assert!(!admin_args(&set).iter().any(|a| a.contains("p4ss")));
        assert_eq!(stdin_of(&op(json!({"op": "clearPassword"}))), None);
    }

    #[test]
    fn the_terminal_form_is_what_a_person_types() {
        let posd = Path::new("/home/me/.local/bin/posd");
        let t = |v, w| terminal_command(&op(v), posd, w);
        assert_eq!(t(json!({"op": "setPassword", "password": "x"}), false), "sudo /home/me/.local/bin/posd admin set-password");
        assert_eq!(t(json!({"op": "autoLock", "minutes": 15}), false), "sudo /home/me/.local/bin/posd admin auto-lock 15");
        // A glob or a query string must survive the shell (Review Focus 4).
        assert_eq!(
            t(json!({"op": "eventsAdd", "url": "https://h/x?token=a&b=it's", "events": ["security.*"]}), false),
            "sudo /home/me/.local/bin/posd admin events add 'https://h/x?token=a&b=it'\\''s' --events 'security.*'"
        );
        assert_eq!(
            terminal_command(&op(json!({"op": "receiptKeyClear"})), Path::new("/opt/My Apps/posd"), false),
            "sudo '/opt/My Apps/posd' admin receipt-key clear"
        );
        assert_eq!(
            terminal_command(&op(json!({"op": "receiptKeyClear"})), Path::new(r"C:\Program Files\Ledger POS\posd.exe"), true),
            r#""C:\Program Files\Ledger POS\posd.exe" admin receipt-key clear"#
        );
    }

    #[test]
    fn a_zero_exit_is_the_json_posd_printed() {
        assert_eq!(
            classify(Some(0), "{\"kid\":\"k1\",\"secret\":\"s\"}\n", "", "cmd"),
            AdminRunResult::Done { output: json!({"kid": "k1", "secret": "s"}) }
        );
        // An older posd without --json prints a usage text: say so rather than show nothing.
        assert!(matches!(classify(Some(0), "Password set.", "", "cmd"), AdminRunResult::Failed { .. }));
    }

    #[test]
    fn a_dismissed_dialog_is_a_cancel_not_an_error() {
        assert_eq!(classify(Some(126), "", "Error executing command as another user: Request dismissed", "cmd"), AdminRunResult::Cancelled);
    }

    #[test]
    fn no_way_to_ask_falls_back_to_the_terminal() {
        match classify(Some(127), "", "Error executing command as another user: No authentication agent found.", "sudo posd admin clear-password") {
            AdminRunResult::Manual { command, reason } => {
                assert_eq!(command, "sudo posd admin clear-password");
                assert!(reason.contains("No authentication agent found"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_refusal_carries_posds_own_words() {
        let newer = "/etc/ledger-pos/admin.json was written by a newer Ledger POS (schema 2; this posd understands 1)";
        assert_eq!(
            classify(Some(2), "", &format!("{newer}\n"), "cmd"),
            AdminRunResult::Failed { message: newer.into(), command: "cmd".into() }
        );
        assert!(matches!(classify(None, "", "", "cmd"), AdminRunResult::Failed { .. }), "killed by a signal");
    }

    /// The program the administrator authorises must be one only root can change —
    /// otherwise whoever shares the desktop account swaps it and the administrator's
    /// password runs their code (review, 2026-10-16).
    #[test]
    fn only_a_root_owned_posd_of_this_version_is_run_as_root() {
        let found = Path::new("/home/me/.local/bin/posd");
        let trusted = |p: &Path| -> Result<(), String> {
            if p.starts_with("/home") { Err(format!("{} is writable by me", p.display())) } else { Ok(()) }
        };
        let exists = |p: &Path| p == found || p == Path::new("/usr/local/bin/posd");
        let v = |p: &Path| Some(if p == Path::new("/usr/local/bin/posd") { "1.1.0".to_string() } else { "1.0.0".into() });

        // The user-owned copy is refused, with the one command that fixes it.
        let none = |_: &Path| false;
        let err = elevation_target(found, &none, &trusted, &v, "1.0.0").unwrap_err();
        assert!(err.contains("sudo install -o root -g root -m 755 /home/me/.local/bin/posd /usr/local/bin/posd"), "{err}");

        // A root-owned copy of another version is not used either (Review: version mismatch).
        let err = elevation_target(found, &exists, &trusted, &v, "1.0.0").unwrap_err();
        assert!(err.contains("1.1.0") && err.contains("1.0.0"), "{err}");

        // The right one is.
        assert_eq!(elevation_target(found, &exists, &trusted, &v, "1.1.0").unwrap(), PathBuf::from("/usr/local/bin/posd"));
        // A package install: posd in /usr/bin, root-owned, found directly.
        let pkg = Path::new("/usr/bin/posd");
        assert_eq!(elevation_target(pkg, &|p: &Path| p == pkg, &trusted, &v, "1.0.0").unwrap(), pkg);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_the_user_can_write_is_not_trusted_and_a_system_binary_is() {
        let d = std::env::temp_dir().join(format!("ledger-pos-trust-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("posd");
        std::fs::write(&f, b"").unwrap();
        assert!(root_only(&f).is_err());
        assert!(root_only(Path::new("/bin/sh")).is_ok(), "root-owned, in root-owned directories");
    }

    #[test]
    fn a_refused_authorisation_is_a_failure_not_a_missing_agent() {
        assert!(matches!(
            classify(Some(127), "", "Error executing command as another user: Not authorized\n", "cmd"),
            AdminRunResult::Failed { .. }
        ));
    }

    #[test]
    fn from_an_appimage_only_the_installed_posd_is_reachable() {
        let d = std::env::temp_dir().join(format!("ledger-pos-elev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("mount/usr/bin")).unwrap();
        std::fs::create_dir_all(d.join("bin")).unwrap();
        let exe = d.join("mount/usr/bin/payment-pos-app");
        std::fs::write(d.join("mount/usr/bin/posd"), b"").unwrap();
        let installed = d.join("bin/posd");
        let appimage = d.join("Ledger.AppImage");

        // Review Focus 3: not installed yet — never the copy inside the FUSE mount.
        let err = posd_for_elevation(&exe, Some(&appimage), &installed).unwrap_err();
        assert!(err.contains("applications menu"), "{err}");

        std::fs::write(&installed, b"").unwrap();
        assert_eq!(posd_for_elevation(&exe, Some(&appimage), &installed).unwrap(), installed);

        // A package install: posd sits next to the app.
        assert_eq!(posd_for_elevation(&exe, None, &installed).unwrap(), d.join("mount/usr/bin/posd"));
        std::fs::remove_file(d.join("mount/usr/bin/posd")).unwrap();
        assert!(posd_for_elevation(&exe, None, &installed).is_err());
    }
}
