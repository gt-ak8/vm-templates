//! `wbox` — microsandbox-backed devstation sandboxes.

#![warn(missing_docs)]

mod build;
mod cli;
mod create;
mod destroy;
mod github;
mod preflight;
mod runtime;
mod ssh;
mod state;

use cli::{Command, HELP};

pub(crate) type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("wbox: {message}\n");
            eprint!("{HELP}");
            std::process::exit(2);
        }
    };
    if matches!(command, Command::Help) {
        print!("{HELP}");
        return;
    }
    if let Err(error) = runtime::install_home() {
        eprintln!("wbox: {error}");
        std::process::exit(1);
    }
    // Both commands that talk to GitHub need microsandbox/.env in the
    // environment: create to register the sandbox key, destroy to remove it.
    // Read it here, while the process is still single-threaded: writing the
    // environment is unsound once the tokio runtime has spawned its workers.
    if matches!(command, Command::Create { .. } | Command::Destroy { .. })
        && let Err(error) = create::load_env_file()
    {
        eprintln!("wbox: {error}");
        std::process::exit(1);
    }
    if let Err(error) = run(command) {
        eprintln!("wbox: {error}");
        std::process::exit(1);
    }
}

fn run(command: Command) -> Res<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(dispatch(command))
}

async fn dispatch(command: Command) -> Res<()> {
    match command {
        Command::Help => Ok(()),
        Command::Build { force, disk_gib } => build::build(force, disk_gib).await,
        Command::Create { name, opts } => create::create(&name, opts).await,
        Command::Destroy { name } => destroy::destroy(&name).await,
        Command::List => runtime::list().await,
        Command::Start { name } => ssh::start(&name).await,
        Command::Stop { name } => ssh::stop(&name).await,
        Command::Ssh { name } => ssh::ssh(&name).await,
    }
}
