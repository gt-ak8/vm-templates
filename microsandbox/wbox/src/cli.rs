//! Hand-rolled argument parsing. No argument-parsing dependency by design.

/// Text printed by `--help` and on any usage error.
pub const HELP: &str = "\
wbox - microsandbox devstation sandboxes

USAGE:
    wbox <command> [args]

COMMANDS:
    build [--force]            build the devstation-base snapshot
    create <name> [options]    create a sandbox from devstation-base
    destroy <name>             remove a sandbox and its GitHub keys
    list                       list the sandboxes wbox created
    ssh <name>                 open an interactive shell in a sandbox
    ssh-proxy <name>           internal: SSH over stdio (ProxyCommand)

CREATE OPTIONS:
    --cpus <n>                 virtual CPUs
    --memory <mib>             memory in MiB

    -h, --help                 print this help
";

/// Resource overrides accepted by `create`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CreateOpts {
    /// Virtual CPU count.
    pub cpus: Option<u8>,
    /// Memory in MiB.
    pub memory: Option<u32>,
}

/// A parsed command line.
#[derive(Debug)]
pub enum Command {
    /// Print the help text.
    Help,
    /// Build the base snapshot.
    Build {
        /// Rebuild even when the snapshot already exists.
        force: bool,
    },
    /// Create a sandbox.
    Create {
        /// Sandbox name.
        name: String,
        /// Resource overrides.
        opts: CreateOpts,
    },
    /// Destroy a sandbox.
    Destroy {
        /// Sandbox name.
        name: String,
    },
    /// List sandboxes.
    List,
    /// Interactive shell.
    Ssh {
        /// Sandbox name.
        name: String,
    },
    /// SSH over stdio.
    SshProxy {
        /// Sandbox name.
        name: String,
    },
}

/// Parse arguments (without the program name). `Err` carries a usage message.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Help);
    };
    let rest = &args[1..];
    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "build" => {
            let mut force = false;
            for arg in rest {
                match arg.as_str() {
                    "--force" => force = true,
                    other => return Err(format!("unknown argument for build: {other}")),
                }
            }
            Ok(Command::Build { force })
        }
        "create" => {
            let mut name: Option<String> = None;
            let mut opts = CreateOpts::default();
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--cpus" => opts.cpus = Some(parse_num(&mut it, "--cpus")?),
                    "--memory" => opts.memory = Some(parse_num(&mut it, "--memory")?),
                    "--disk" => {
                        return Err(
                            "--disk is not supported: a sandbox inherits the root disk \
                             size of the base snapshot"
                                .to_string(),
                        );
                    }
                    other if other.starts_with('-') => {
                        return Err(format!("unknown argument for create: {other}"));
                    }
                    other if name.is_none() => name = Some(other.to_string()),
                    other => return Err(format!("unexpected argument for create: {other}")),
                }
            }
            let name = name.ok_or_else(|| "create needs a sandbox name".to_string())?;
            Ok(Command::Create { name, opts })
        }
        "destroy" => Ok(Command::Destroy {
            name: one_name(rest, "destroy")?,
        }),
        "list" => {
            if let Some(other) = rest.first() {
                return Err(format!("unknown argument for list: {other}"));
            }
            Ok(Command::List)
        }
        "ssh" => Ok(Command::Ssh {
            name: one_name(rest, "ssh")?,
        }),
        "ssh-proxy" => Ok(Command::SshProxy {
            name: one_name(rest, "ssh-proxy")?,
        }),
        other => Err(format!("unknown command: {other}")),
    }
}

fn one_name(rest: &[String], cmd: &str) -> Result<String, String> {
    match rest {
        [name] if !name.starts_with('-') => Ok(name.clone()),
        [] => Err(format!("{cmd} needs a sandbox name")),
        _ => Err(format!("{cmd} takes exactly one sandbox name")),
    }
}

fn parse_num<'a, T: std::str::FromStr>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<T, String> {
    let raw = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
    raw.parse::<T>()
        .map_err(|_| format!("{flag} needs a positive number, got {raw}"))
}
