//! Hand-rolled argument parsing. No argument-parsing dependency by design.

/// Text printed by `--help` and on any usage error.
pub const HELP: &str = "\
wbox - microsandbox devstation sandboxes

USAGE:
    wbox <command> [args]

COMMANDS:
    build [--force] [--disk g] build the devstation-base snapshot
    create <name> [options]    create a sandbox from devstation-base
    destroy <name>             remove a sandbox and its GitHub keys
    list                       list the sandboxes wbox created
    start <name>               boot a stopped sandbox and its sshd
    stop <name>                stop a sandbox, keeping it for start
    ssh <name>                 shell into a running sandbox (= ssh wbox-<name>)

BUILD OPTIONS:
    --disk <gib>               root disk in GiB, baked into the snapshot and
                               inherited by every sandbox (default 50)

CREATE OPTIONS:
    --cpus <n>                 virtual CPUs (default 4)
    --memory <mib>             memory in MiB (default 8192)
                               4096 = 4 GiB, 8192 = 8 GiB, 16384 = 16 GiB
    --ssh-port <port>          host port for ssh, on 127.0.0.1 only
                               (default: first free one from 22200)
    --metrics                  sample runtime metrics (`msb metrics`) every
                               second. Off by default: the sampler stalls all
                               host<->guest I/O for 100-190 ms per sample

    -h, --help                 print this help
";

/// Default resources when `--cpus/--memory` are not given. The root disk is
/// not among them: it is a property of the OCI rootfs source, so a
/// snapshot-rooted sandbox inherits the size baked in at build time and the
/// SDK rejects setting it here.
pub const DEFAULT_CPUS: u8 = 4;
pub const DEFAULT_MEMORY_MIB: u32 = 8192;

/// Root disk of the base snapshot, and so of every sandbox, when `build` is
/// not given `--disk`.
pub const DEFAULT_DISK_GIB: u32 = 50;

/// First host port `create` tries for sshd when `--ssh-port` is not given.
pub const SSH_PORT_BASE: u16 = 22200;

/// Resource overrides accepted by `create`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CreateOpts {
    /// Virtual CPU count.
    pub cpus: Option<u8>,
    /// Memory in MiB.
    pub memory: Option<u32>,
    /// Host loopback port to publish the guest's sshd on.
    pub ssh_port: Option<u16>,
    /// Keep the runtime's metrics sampler on (it is disabled otherwise).
    pub metrics: bool,
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
        /// Root disk size in GiB.
        disk_gib: u32,
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
    /// Boot a stopped sandbox.
    Start {
        /// Sandbox name.
        name: String,
    },
    /// Stop a sandbox without removing it.
    Stop {
        /// Sandbox name.
        name: String,
    },
    /// Interactive shell.
    Ssh {
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
            let mut disk_gib = DEFAULT_DISK_GIB;
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--force" => force = true,
                    "--disk" => disk_gib = parse_num(&mut it, "--disk")?,
                    other => return Err(format!("unknown argument for build: {other}")),
                }
            }
            if disk_gib == 0 {
                return Err("--disk needs a positive number of GiB".to_string());
            }
            Ok(Command::Build { force, disk_gib })
        }
        "create" => {
            let mut name: Option<String> = None;
            let mut opts = CreateOpts::default();
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--cpus" => opts.cpus = Some(parse_num(&mut it, "--cpus")?),
                    "--memory" => opts.memory = Some(parse_num(&mut it, "--memory")?),
                    "--ssh-port" => opts.ssh_port = Some(parse_num(&mut it, "--ssh-port")?),
                    "--metrics" => opts.metrics = true,
                    "--disk" => {
                        return Err("--disk is not supported: a sandbox inherits the root disk \
                             size of the base snapshot"
                            .to_string());
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
        "start" => Ok(Command::Start {
            name: one_name(rest, "start")?,
        }),
        "stop" => Ok(Command::Stop {
            name: one_name(rest, "stop")?,
        }),
        "ssh" => Ok(Command::Ssh {
            name: one_name(rest, "ssh")?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_states_the_create_defaults() {
        assert!(HELP.contains(&format!("virtual CPUs (default {DEFAULT_CPUS})")));
        assert!(HELP.contains(&format!("memory in MiB (default {DEFAULT_MEMORY_MIB})")));
        assert!(HELP.contains(&format!("first free one from {SSH_PORT_BASE}")));
        assert!(HELP.contains(&format!("(default {DEFAULT_DISK_GIB})")));
    }

    #[test]
    fn build_takes_a_disk_size_in_gib() {
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        match parse(&args("build")) {
            Ok(Command::Build { disk_gib, .. }) => assert_eq!(disk_gib, DEFAULT_DISK_GIB),
            other => panic!("{other:?}"),
        }
        match parse(&args("build --force --disk 80")) {
            Ok(Command::Build { force, disk_gib }) => assert!(force && disk_gib == 80),
            other => panic!("{other:?}"),
        }
        assert!(parse(&args("build --disk 0")).is_err());
    }

    #[test]
    fn parses_start_stop_and_the_ssh_port() {
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert!(matches!(parse(&args("start a")), Ok(Command::Start { name }) if name == "a"));
        assert!(matches!(parse(&args("stop a")), Ok(Command::Stop { name }) if name == "a"));
        assert!(parse(&args("stop a b")).is_err());
        match parse(&args("create a --ssh-port 22210")) {
            Ok(Command::Create { opts, .. }) => {
                assert_eq!(opts.ssh_port, Some(22210));
                assert!(!opts.metrics);
            }
            other => panic!("{other:?}"),
        }
        match parse(&args("create a --metrics")) {
            Ok(Command::Create { opts, .. }) => assert!(opts.metrics),
            other => panic!("{other:?}"),
        }
        assert!(parse(&args("ssh-proxy a")).is_err());
    }
}
