mod cli_workflows;

const HELP: &str = "sterna — a coding agent for your terminal

Usage:
  sterna
  sterna -p <task> [session options]
  sterna exec [task] [session options]
  sterna --resume [id] | --continue | --sessions
  sterna doctor [--root <path>] [--json]
  sterna update [--check]
  sterna config [global|local] [key] [value] [--root <path>]
  sterna session --root <path> [options]
  sterna ruler run [options]
  sterna --help
  sterna --version

Running `sterna` with no arguments starts a session in the current project.
`exec` without a task reads all of stdin as one task. Piped input to ordinary
sessions remains one turn per line. Session options default --root to .";

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `sterna --version` prints the crate version and nothing else: a release
    // archive can be told from a build, and the primary asked for it (07:16).
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("sterna {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        println!("{HELP}");
        return Ok(());
    }
    if args.is_empty() {
        return dispatch_session(&["--root".into(), ".".into()]);
    }
    if args.first().map(String::as_str) == Some("ruler") {
        sterna::relocate::announce(None);
        if let Err(message) = sterna::ruler::cli::dispatch(&args[1..]) {
            eprintln!("{message}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("session") {
        return dispatch_session(&cli_workflows::session_options(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("update") {
        sterna::relocate::announce(None);
        std::process::exit(sterna::update::command(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("doctor") {
        sterna::relocate::announce(Some(&sterna::relocate::root_of(&args[1..])));
        std::process::exit(cli_workflows::doctor(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("config") {
        sterna::relocate::announce(Some(&sterna::relocate::root_of(&args[1..])));
        match sterna::settings_commands::cli(&args[1..]) {
            Ok(message) => println!("{message}"),
            Err(message) => {
                eprintln!("sterna config: {message}");
                std::process::exit(2);
            }
        }
        return Ok(());
    }
    match cli_workflows::prepare(&args) {
        Ok(Some(args)) => return dispatch_session(&args),
        Ok(None) => {}
        Err(message) => {
            eprintln!("sterna: {message}");
            std::process::exit(2);
        }
    }

    let kind = if args[0].starts_with('-') {
        "option"
    } else {
        "command"
    };
    eprintln!("sterna: unknown {kind}: {}", args[0]);
    eprintln!("Try 'sterna --help' for usage.");
    std::process::exit(2);
}

fn dispatch_session(args: &[String]) -> std::io::Result<()> {
    if let Err(message) = sterna::session::dispatch(args) {
        eprintln!("{message}");
        std::process::exit(1);
    }
    Ok(())
}
