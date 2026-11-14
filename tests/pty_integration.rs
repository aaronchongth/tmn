use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc;
use std::time::Duration;
use tmn::app::{App, AppAction, ExitOutcome, SignalReq};
use tmn::pty::{PtyConfig, PtyEvent, PtySession};

#[test]
fn pty_sets_tty_and_default_env_without_clobbering_caller() {
    let (tx, rx) = mpsc::channel();
    let config = PtyConfig {
        command: vec![
            "python3".to_string(),
            "-c".to_string(),
            "import os, sys; print(sys.stdout.isatty(), os.environ.get('OVERRIDE_LAUNCH_PROCESS_OUTPUT'))".to_string(),
        ],
        inject_default_env: true,
        cols: 80,
        rows: 24,
    };

    let _session = PtySession::spawn(config, tx).unwrap();
    let mut lines = Vec::new();
    let mut exit_code = None;

    while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
        match event {
            PtyEvent::Line(l) => lines.push(l),
            PtyEvent::Exited(outcome) => {
                exit_code = Some(outcome);
                break;
            }
        }
    }

    assert_eq!(exit_code, Some(ExitOutcome::Code(0)));
    assert!(!lines.is_empty());
    assert_eq!(lines[0].trim(), "True both");
}

#[test]
fn pty_flushes_unterminated_final_line_on_slave_close_eio() {
    let (tx, rx) = mpsc::channel();
    let config = PtyConfig {
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'Segmentation fault'".to_string(),
        ],
        inject_default_env: true,
        cols: 80,
        rows: 24,
    };

    let _session = PtySession::spawn(config, tx).unwrap();
    let mut lines = Vec::new();
    let mut exit_code = None;

    while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
        match event {
            PtyEvent::Line(l) => lines.push(l),
            PtyEvent::Exited(outcome) => {
                exit_code = Some(outcome);
                break;
            }
        }
    }

    assert_eq!(exit_code, Some(ExitOutcome::Code(0)));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], "Segmentation fault");
}

#[test]
fn ctrl_c_forwards_sigint_and_holds_post_mortem() {
    let (tx, rx) = mpsc::channel();
    let config = PtyConfig {
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            // Poll with short sleeps rather than one long `sleep`: POSIX shells defer
            // traps until the foreground command finishes, so if SIGINT lands just
            // before the shell forks a long `sleep` the trap would not run for ages.
            "trap \"echo caught_int; exit 0\" INT; echo ready; while :; do sleep 0.1; done"
                .to_string(),
        ],
        inject_default_env: true,
        cols: 80,
        rows: 24,
    };

    let session = PtySession::spawn(config, tx).unwrap();
    let mut app = App::new("sh".to_string(), 1000);

    // Wait for "ready"
    loop {
        if let Ok(PtyEvent::Line(l)) = rx.recv_timeout(Duration::from_secs(5)) {
            #[allow(clippy::collapsible_if)]
            if l.contains("ready") {
                break;
            }
        }
    }

    let key_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let action = app.on_key(key_c);

    assert_eq!(action, AppAction::SignalChild(SignalReq::Int));
    session.send_signal(SignalReq::Int);

    // Should receive caught_int and Exited
    let mut caught = false;
    let mut exited = false;

    while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
        match event {
            PtyEvent::Line(l) => {
                if l.contains("caught_int") {
                    caught = true;
                }
            }
            PtyEvent::Exited(outcome) => {
                assert_eq!(outcome, ExitOutcome::Code(0));
                let action = app.on_child_exited(outcome);
                assert_eq!(action, AppAction::None); // Post-mortem hold
                exited = true;
                break;
            }
        }
    }

    assert!(caught);
    assert!(exited);

    // Test q quits
    let key_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    let action = app.on_key(key_q);
    assert_eq!(action, AppAction::ExitTui);
}

#[test]
fn quitting_escalates_to_sigkill_when_child_ignores_sigint() {
    let (tx, rx) = mpsc::channel();
    let config = PtyConfig {
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            "trap \"\" INT TERM; echo stubborn; sleep 60".to_string(),
        ],
        inject_default_env: true,
        cols: 80,
        rows: 24,
    };

    let session = PtySession::spawn(config, tx).unwrap();
    let mut app = App::new("sh".to_string(), 1000);

    // Wait for "stubborn"
    loop {
        if let Ok(PtyEvent::Line(l)) = rx.recv_timeout(Duration::from_secs(5)) {
            #[allow(clippy::collapsible_if)]
            if l.contains("stubborn") {
                break;
            }
        }
    }

    // Hit q -> escalates to IntSent
    let key_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    let action = app.on_key(key_q);
    assert_eq!(action, AppAction::SignalChild(SignalReq::Int));
    session.send_signal(SignalReq::Int);

    // Trigger timeout -> escalates to TermSent
    let action = app.on_quit_timeout();
    assert_eq!(action, AppAction::SignalChild(SignalReq::Term));
    session.send_signal(SignalReq::Term);

    // Trigger timeout -> escalates to KillSent
    let action = app.on_quit_timeout();
    assert_eq!(action, AppAction::SignalChild(SignalReq::Kill));
    session.send_signal(SignalReq::Kill);

    // Wait for exit
    let mut exit_outcome = None;
    while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
        if let PtyEvent::Exited(outcome) = event {
            exit_outcome = Some(outcome);
            break;
        }
    }

    assert_eq!(exit_outcome, Some(ExitOutcome::Signal(9)));

    // Check ESRCH (no orphan)
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let err = kill(Pid::from_raw(session.child_pid()), None).unwrap_err();
    assert_eq!(err, nix::errno::Errno::ESRCH);
}

#[test]
fn headless_binary_runs_end_to_end_and_propagates_exit_code() {
    use std::process::Command;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tmn"));
    cmd.args([
        "--headless",
        "sh",
        "-c",
        "echo '[node-1] [INFO] [1.0] [node]: hi'; exit 7",
    ]);

    let output = cmd.output().expect("Failed to execute headless binary");

    assert_eq!(output.status.code(), Some(7));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hi"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("node-1"));
}
