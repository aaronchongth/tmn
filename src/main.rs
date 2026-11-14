use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tmn::app::{App, AppAction, ExitOutcome};
use tmn::pty::{PtyConfig, PtyEvent, PtySession};
use tmn::ui::{TerminalGuard, render};

fn run_interactive(mut app: App, config: PtyConfig) -> i32 {
    let (tx, rx) = mpsc::channel();
    let session = match PtySession::spawn(config, tx) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    let mut guard = match TerminalGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Failed to enter terminal mode: {}", e);
            return 1;
        }
    };

    let mut last_pty_size = (0u16, 0u16);
    let _ = guard.terminal_mut().draw(|f| render(f, &mut app));
    let vp = app.last_log_viewport_size();
    if vp.0 > 0 && vp.1 > 0 && vp != last_pty_size {
        session.resize(vp.0, vp.1);
        last_pty_size = vp;
    }

    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, std::sync::Arc::clone(&flag));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, std::sync::Arc::clone(&flag));

    let mut last_quit_escalation = Instant::now();
    let tick_rate = Duration::from_millis(16);
    let mut last_tick = Instant::now();

    loop {
        if flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let action = app.request_external_shutdown();
            if handle_action(&session, action, &mut last_quit_escalation) {
                break;
            }
        }

        while let Ok(event) = rx.try_recv() {
            match event {
                PtyEvent::Line(line) => {
                    app.on_line(&line);
                }
                PtyEvent::Exited(outcome) => {
                    let action = app.on_child_exited(outcome);
                    if handle_action(&session, action, &mut last_quit_escalation) {
                        break;
                    }
                }
            }
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if crossterm::event::poll(timeout).unwrap_or(false) {
            #[allow(clippy::collapsible_if)]
            if let Ok(event) = crossterm::event::read() {
                match event {
                    crossterm::event::Event::Key(key) => {
                        if key.kind == crossterm::event::KeyEventKind::Press {
                            let action = app.on_key(key);
                            if handle_action(&session, action, &mut last_quit_escalation) {
                                break;
                            }
                            let _ = guard.terminal_mut().draw(|f| render(f, &mut app));
                            let vp = app.last_log_viewport_size();
                            if vp.0 > 0 && vp.1 > 0 && vp != last_pty_size {
                                session.resize(vp.0, vp.1);
                                last_pty_size = vp;
                            }
                        }
                    }
                    crossterm::event::Event::Resize(_c, _r) => {
                        let _ = guard.terminal_mut().draw(|f| render(f, &mut app));
                        let vp = app.last_log_viewport_size();
                        if vp.0 > 0 && vp.1 > 0 && vp != last_pty_size {
                            session.resize(vp.0, vp.1);
                            last_pty_size = vp;
                        }
                    }
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();

            match app.child_state() {
                tmn::app::ChildState::Quitting {
                    stage: tmn::app::QuitStage::IntSent,
                } => {
                    if last_quit_escalation.elapsed() >= Duration::from_secs(5) {
                        let action = app.on_quit_timeout();
                        if handle_action(&session, action, &mut last_quit_escalation) {
                            break;
                        }
                    }
                }
                tmn::app::ChildState::Quitting {
                    stage: tmn::app::QuitStage::TermSent,
                } if last_quit_escalation.elapsed() >= Duration::from_secs(2) => {
                    let action = app.on_quit_timeout();
                    if handle_action(&session, action, &mut last_quit_escalation) {
                        break;
                    }
                }
                _ => {}
            }

            let _ = guard.terminal_mut().draw(|f| render(f, &mut app));
            let vp = app.last_log_viewport_size();
            if vp.0 > 0 && vp.1 > 0 && vp != last_pty_size {
                session.resize(vp.0, vp.1);
                last_pty_size = vp;
            }
        }
    }

    0
}

fn handle_action(session: &PtySession, action: AppAction, last_escalation: &mut Instant) -> bool {
    match action {
        AppAction::None => false,
        AppAction::SignalChild(req) => {
            session.send_signal(req);
            *last_escalation = Instant::now();
            false
        }
        AppAction::ExitTui => true,
    }
}

fn main() -> ExitCode {
    match tmn::cli::parse_args(std::env::args_os()) {
        Ok(tmn::cli::CliOutcome::Run(cli)) => {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let (init_cols, init_rows) = if cli.headless {
                (cols, rows)
            } else {
                (
                    cols.saturating_sub(28).max(1),
                    rows.saturating_sub(2).max(1),
                )
            };
            let config = PtyConfig {
                command: cli.command,
                inject_default_env: !cli.no_env_override,
                cols: init_cols,
                rows: init_rows,
            };

            if cli.headless {
                let (tx, rx) = mpsc::channel();
                let _session = match PtySession::spawn(config, tx) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{}", e);
                        return ExitCode::from(1);
                    }
                };

                let mut store = tmn::store::LogStore::new(cli.max_lines);
                let mut final_outcome = ExitOutcome::Code(1);

                while let Ok(event) = rx.recv() {
                    match event {
                        PtyEvent::Line(line) => {
                            store.ingest_line(&line);
                            println!("{}", line); // Print captured display lines to stdout
                        }
                        PtyEvent::Exited(outcome) => {
                            final_outcome = outcome;
                            break;
                        }
                    }
                }

                // Print a per-source summary to stderr
                let rows = store.source_rows();
                eprintln!("\n--- Headless Summary ---");
                for row in rows {
                    if let Some(tmn::store::SourceTarget::Process(_pid)) = row.target {
                        eprintln!("Source: {}", row.label);
                    }
                }

                ExitCode::from((final_outcome.shell_exit_code() & 0xff) as u8)
            } else {
                let app = App::new(config.command.join(" "), cli.max_lines);
                let code = run_interactive(app, config);
                ExitCode::from(code as u8)
            }
        }
        Ok(tmn::cli::CliOutcome::Printed(s)) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            if e.message.ends_with('\n') {
                eprint!("{}", e.message);
            } else {
                eprintln!("{}", e.message);
            }
            ExitCode::from(e.exit_code as u8)
        }
    }
}
