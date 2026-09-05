use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{exit, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

// Set right after spawning the ssh child so the signal handler below can
// reach it. 0 means "no child to clean up yet".
static CHILD_PID: AtomicI32 = AtomicI32::new(0);

extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
    fn kill(pid: i32, sig: i32) -> i32;
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

// Runs the child ssh explicitly instead of relying on it sharing tunnel's
// process group: a plain `kill <tunnel-pid>` (as opposed to a real Ctrl+C at
// the terminal, which signals the whole foreground process group) only hits
// this process, and without this the ssh child would be orphaned and keep
// holding the port - confirmed by testing.
extern "C" fn handle_termination(sig: i32) {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            kill(pid, sig);
        }
    }
    exit(128 + sig);
}

fn usage() -> ! {
    eprintln!("usage:");
    eprintln!("  tunnel <ssh-host-alias> <port>   forward local:PORT -> alias:PORT");
    eprintln!("  tunnel -kill <port>              kill whatever is listening on PORT locally");
    exit(2);
}

fn parse_port(s: &str) -> u16 {
    match s.parse() {
        Ok(p) if p > 0 => p,
        _ => {
            eprintln!("error: port tidak valid: {}", s);
            exit(2);
        }
    }
}

struct PortOwner {
    pid: String,
    command: String,
    user: String,
    full_cmd: String,
}

fn ssh_config_path() -> Option<PathBuf> {
    env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh/config"))
}

fn warn_if_host_unknown(host: &str) {
    let path = match ssh_config_path() {
        Some(p) => p,
        None => return,
    };
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let keyword = match parts.next() {
            Some(k) => k,
            None => continue,
        };
        if keyword.eq_ignore_ascii_case("host") && parts.any(|tok| tok == host) {
            return;
        }
    }

    eprintln!(
        "warning: host \"{}\" tidak ditemukan langsung di ~/.ssh/config \
         (mungkin dari Include atau pattern) - tetap mencoba connect.",
        host
    );
}

fn full_command_line(pid: &str) -> Option<String> {
    let output = Command::new("ps").args(["-p", pid, "-o", "command="]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn port_owners(port: u16) -> Vec<PortOwner> {
    let output = match Command::new("lsof")
        .arg("-nP")
        .arg(format!("-iTCP:{}", port))
        .arg("-sTCP:LISTEN")
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        // lsof exits non-zero when nothing matches - port is free.
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut seen_pids: Vec<String> = Vec::new();
    let mut result = Vec::new();

    for line in text.lines().skip(1) {
        // skip the "COMMAND PID USER FD TYPE ..." header
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let command = fields[0].to_string();
        let pid = fields[1].to_string();
        let user = fields[2].to_string();

        if seen_pids.contains(&pid) {
            continue; // same process can show up twice (IPv4 + IPv6 sockets)
        }
        seen_pids.push(pid.clone());

        let full_cmd = full_command_line(&pid).unwrap_or_else(|| command.clone());
        result.push(PortOwner { pid, command, user, full_cmd });
    }

    result
}

fn port_owner(port: u16) -> Option<PortOwner> {
    port_owners(port).into_iter().next()
}

fn print_port_in_use_error(port: u16, info: &PortOwner) {
    eprintln!("error: port {} sudah dipakai di local, tunnel dibatalkan.", port);
    eprintln!();
    eprintln!("  PID     : {}", info.pid);
    eprintln!("  Process : {}", info.command);
    eprintln!("  User    : {}", info.user);
    eprintln!("  Command : {}", info.full_cmd);
    eprintln!();
    eprintln!("Kill manual kalau memang mau lanjut:");
    eprintln!("  kill {}", info.pid);
}

fn run_kill(port: u16) {
    let owners = port_owners(port);
    if owners.is_empty() {
        eprintln!("tidak ada proses yang memakai port {} di local.", port);
        exit(1);
    }

    let mut any_failed = false;
    for owner in &owners {
        eprint!(
            "killing PID {} ({}, user {}) - {} ... ",
            owner.pid, owner.command, owner.user, owner.full_cmd
        );
        match Command::new("kill").arg(&owner.pid).status() {
            Ok(status) if status.success() => eprintln!("ok"),
            Ok(status) => {
                any_failed = true;
                eprintln!("gagal (exit {})", status.code().unwrap_or(-1));
            }
            Err(e) => {
                any_failed = true;
                eprintln!("gagal ({})", e);
            }
        }
    }

    exit(if any_failed { 1 } else { 0 });
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() == 3 && (args[1] == "-kill" || args[1] == "--kill") {
        let port = parse_port(&args[2]);
        run_kill(port);
    }

    if args.len() != 3 {
        usage();
    }

    let host = &args[1];
    let port: u16 = parse_port(&args[2]);

    warn_if_host_unknown(host);

    if let Some(info) = port_owner(port) {
        print_port_in_use_error(port, &info);
        exit(1);
    }

    eprintln!("tunnel: {port} (local) -> {host}:{port} (remote) -- Ctrl+C untuk berhenti");

    // Piped stderr (instead of exec()) so repeated identical lines - e.g. ssh
    // logging "channel N: open failed: connect failed: Connection refused"
    // once per rejected connection attempt when nothing listens on the
    // remote port yet - can be collapsed instead of spamming the terminal.
    let mut child = match Command::new("ssh")
        .arg("-N")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-L")
        .arg(format!("{port}:localhost:{port}"))
        .arg(host)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: gagal menjalankan ssh: {}", e);
            exit(1);
        }
    };

    CHILD_PID.store(child.id() as i32, Ordering::SeqCst);
    unsafe {
        signal(SIGINT, handle_termination as *const () as usize);
        signal(SIGTERM, handle_termination as *const () as usize);
    }

    let stderr = child.stderr.take().expect("stderr was piped");
    let mut last_line: Option<String> = None;
    let mut repeat_count: u32 = 0;

    for line in BufReader::new(stderr).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if last_line.as_deref() == Some(line.as_str()) {
            repeat_count += 1;
            continue;
        }
        if repeat_count > 0 {
            eprintln!("  (baris sebelumnya berulang {} kali lagi, disembunyikan)", repeat_count);
        }
        eprintln!("{line}");
        last_line = Some(line);
        repeat_count = 0;
    }
    if repeat_count > 0 {
        eprintln!("  (baris sebelumnya berulang {} kali lagi, disembunyikan)", repeat_count);
    }

    let status = child.wait().unwrap_or_else(|e| {
        eprintln!("error: gagal menunggu ssh: {}", e);
        exit(1);
    });
    exit(status.code().unwrap_or(1));
}
