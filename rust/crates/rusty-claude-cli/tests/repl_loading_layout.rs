use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn repl_keeps_working_indicator_above_composer_and_model_footer() {
    let workspace = unique_temp_dir("repl-loading-layout");
    let config_home = workspace.join("config-home");
    let home = workspace.join("home");
    fs::create_dir_all(&config_home).expect("config home should exist");
    fs::create_dir_all(&home).expect("home should exist");

    let output = run_claw_repl(&workspace, &config_home, &home);
    assert!(
        output.status.success(),
        "PTY harness should complete successfully\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let terminal_output = String::from_utf8_lossy(&output.stdout);
    let working_region = terminal_output.find("\x1b[1;22r").unwrap_or_else(|| {
        panic!("working state should set the transcript scroll region: {terminal_output:?}")
    });
    let reserved_output = &terminal_output[working_region..];
    let composer = working_region
        + reserved_output
            .find("> ")
            .expect("working state should redraw the composer");
    let model_footer = terminal_output
        .rfind("• anthropic/claude-sonnet-4-6")
        .expect("working state should redraw the model footer");
    let working = terminal_output
        .find("Working")
        .expect("working state should render the activity indicator");

    assert!(
        working_region < composer,
        "working state should establish the reserved row before drawing the composer: {terminal_output:?}"
    );
    assert!(
        composer < model_footer,
        "composer should remain above the model footer: {terminal_output:?}"
    );
    assert!(
        model_footer < working,
        "activity should render after the cursor is moved to the row above the composer: {terminal_output:?}"
    );
    let activity_move = terminal_output[..working]
        .rfind("\x1b[22;1H")
        .expect("activity frame should move to the reserved working row");
    let cursor_save = terminal_output[..activity_move]
        .rfind("\x1b7")
        .expect("activity frame should save the composer cursor");
    assert!(
        cursor_save < activity_move && terminal_output[working..].contains("\x1b8"),
        "activity redraw should restore the composer cursor after painting the working row: {terminal_output:?}"
    );
    assert!(
        terminal_output.contains("0 tok"),
        "model footer should include live token usage: {terminal_output:?}"
    );

    fs::remove_dir_all(&workspace).expect("workspace cleanup should succeed");
}

fn run_claw_repl(
    cwd: &std::path::Path,
    config_home: &std::path::Path,
    home: &std::path::Path,
) -> std::process::Output {
    python_pty_command(env!("CARGO_BIN_EXE_clawcli"))
        .current_dir(cwd)
        .env_clear()
        .env("ANTHROPIC_API_KEY", "test-repl-loading-key")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .env("CLAW_CONFIG_HOME", config_home)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("PTY harness should finish")
}

fn python_pty_command(claw: &str) -> Command {
    let mut command = Command::new("python3");
    command.args([
        "-c",
        r#"
import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time

claw = sys.argv[1]
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
child = subprocess.Popen([claw, "--model", "anthropic/claude-sonnet-4-6"],
                         stdin=slave, stdout=slave, stderr=slave,
                         start_new_session=True)
os.close(slave)
captured = bytearray()

def drain(seconds):
    deadline = time.time() + seconds
    while time.time() < deadline:
        readable, _, _ = select.select([master], [], [], min(0.1, deadline - time.time()))
        if not readable:
            continue
        try:
            captured.extend(os.read(master, 65536))
        except OSError:
            break

# Give rustyline enough time to put the initial composer on the PTY, then
# submit a prompt. The local refused port keeps the model request offline.
drain(1.5)
os.write(master, b"hello?\r")
drain(3.0)

if child.poll() is None:
    child.terminate()
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()

os.close(master)
sys.stdout.buffer.write(captured)
sys.stdout.buffer.flush()
"#,
        claw,
    ]);
    command
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_millis();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "claw-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}
