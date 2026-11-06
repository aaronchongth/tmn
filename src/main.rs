use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tmn::app::{App, AppAction, ExitOutcome};
use tmn::pty::{PtyConfig, PtyEvent, PtySession};
use tmn::ui::{render, TerminalGuard};

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

    let (mut cols, mut rows) = crossterm::terminal::size().unwrap_or((80, 24));
    session.resize(cols, rows);

    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, std::sync::Arc::clone(&flag));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, std::sync::Arc::clone(&flag));

    let mut last_quit_escalation = Instant::now();
    let tick_rate = Duration::from_millis(16);
    let mut last_tick = Instant::now();

    loop {
        if flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let action = app.request_external_shutdown();
            handle_action(&session, action, &mut last_quit_escalation);
        }

        while let Ok(event) = rx.try_recv() {
            match event {
                PtyEvent::Line(line) => {
                    app.on_line(&line);
                }
                PtyEvent::Exited(outcome) => {
                    let action = app.on_child_exited(outcome);
                    handle_action(&session, action, &mut last_quit_escalation);
                }
            }
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if crossterm::event::poll(timeout).unwrap_or(false) {
            if let Ok(crossterm::event::Event::Key(key)) = crossterm::event::read() {
                let action = app.on_key(key);
                handle_action(&session, action, &mut last_quit_escalation);
            } else if let Ok(crossterm::event::Event::Resize(c, r)) = crossterm::event::read() {
                cols = c;
                rows = r;
                session.resize(cols, rows);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
            let action = app.on_quit_timeout();
            handle_action(&session, action, &mut last_quit_escalation);
            
            let _ = guard.terminal_mut().draw(|f| render(f, &mut app));
        }
    }
}

fn handle_action(session: &PtySession, action: AppAction, last_escalation: &mut Instant) {
    match action {
        AppAction::None => {}
        AppAction::SignalChild(req) => {
            session.send_signal(req);
            *last_escalation = Instant::now();
        }
        AppAction::ExitTui => {
            std::process::exit(0);
        }
    }
}

fn main() -> ExitCode {
    match tmn::cli::parse_args(std::env::args_os()) {
        Ok(tmn::cli::CliOutcome::Run(cli)) => {
            let config = PtyConfig {
                command: cli.command,
                inject_default_env: !cli.no_env_override,
                cols: 80, // will be updated via resize in interactive mode
                rows: 24,
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
                let app = App::new(String::new(), cli.max_lines);
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
