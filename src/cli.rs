//! Command-line entry points beside the TUI: `lazyssh <name>`, `ls`,
//! `cmd`, and `import`. Parsing is pure so it can be tested directly.

use crate::config::{Config, Server};
use crate::ssh::LaunchMode;

pub const USAGE: &str = "\
lazyssh — a tiny TUI for your SSH servers

USAGE:
    lazyssh                        open the TUI
    lazyssh <name> [--sftp|--mosh] connect by name (exact, else unique prefix)
    lazyssh connect <name> [...]   same, for servers named like a subcommand
    lazyssh ls [--tag <tag>]       list servers: name, target, tags
    lazyssh cmd <name>             print the ssh command line for a server
    lazyssh import [PATH]          import new hosts from ~/.ssh/config (or PATH)
    lazyssh --help | --version
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Tui,
    Help,
    Version,
    Connect { name: String, mode: LaunchMode },
    List { tag: Option<String> },
    Print { name: String },
    Import { path: Option<String> },
}

/// Parses `args` (without the program name).
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut args = args.iter().map(String::as_str);
    let Some(first) = args.next() else {
        return Ok(Command::Tui);
    };
    let rest: Vec<&str> = args.collect();
    let none_left = |cmd: Command, rest: &[&str]| match rest.first() {
        None => Ok(cmd),
        Some(extra) => Err(format!("unexpected argument `{extra}`")),
    };
    match first {
        "-h" | "--help" | "help" => none_left(Command::Help, &rest),
        "-V" | "--version" => none_left(Command::Version, &rest),
        "ls" | "list" => match rest.as_slice() {
            [] => Ok(Command::List { tag: None }),
            ["--tag" | "-t", tag] => Ok(Command::List {
                tag: Some(tag.trim_start_matches('#').to_string()),
            }),
            _ => Err("usage: lazyssh ls [--tag <tag>]".into()),
        },
        "cmd" => match rest.as_slice() {
            [name] => Ok(Command::Print {
                name: name.to_string(),
            }),
            _ => Err("usage: lazyssh cmd <name>".into()),
        },
        "import" => match rest.as_slice() {
            [] => Ok(Command::Import { path: None }),
            [path] => Ok(Command::Import {
                path: Some(path.to_string()),
            }),
            _ => Err("usage: lazyssh import [PATH]".into()),
        },
        "connect" => match rest.split_first() {
            Some((name, flags)) => connect(name, flags),
            None => Err("usage: lazyssh connect <name> [--sftp|--mosh]".into()),
        },
        flag if flag.starts_with('-') => Err(format!("unknown option `{flag}`")),
        name => connect(name, &rest),
    }
}

fn connect(name: &str, flags: &[&str]) -> Result<Command, String> {
    let mode = match flags {
        [] => LaunchMode::Ssh,
        ["--sftp"] => LaunchMode::Sftp,
        ["--mosh"] => LaunchMode::Mosh,
        [other, ..] => return Err(format!("unexpected argument `{other}`")),
    };
    Ok(Command::Connect {
        name: name.to_string(),
        mode,
    })
}

/// Finds a server by exact name, else by unique name prefix (both ignoring
/// case). Ambiguous prefixes list the candidates instead of guessing.
pub fn find_server(config: &Config, query: &str) -> Result<usize, String> {
    if let Some(index) = config.index_of_name(query) {
        return Ok(index);
    }
    let query_lower = query.to_lowercase();
    let matches: Vec<usize> = config
        .servers
        .iter()
        .enumerate()
        .filter(|(_, s)| s.name.to_lowercase().starts_with(&query_lower))
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no server named `{query}` (try `lazyssh ls`)")),
        many => {
            let names: Vec<&str> = many
                .iter()
                .map(|&i| config.servers[i].name.as_str())
                .collect();
            Err(format!("`{query}` is ambiguous: {}", names.join(", ")))
        }
    }
}

/// One tab-separated `name  target  tags` row per server, for scripts and
/// fzf. Rows keep the TUI's pinned-then-recent order.
pub fn list_rows(config: &Config, tag: Option<&str>) -> Vec<String> {
    let mut sorted = config.clone();
    sorted.sort_by_recency();
    sorted
        .servers
        .iter()
        .filter(|s| tag.is_none_or(|t| s.tags.iter().any(|x| x.eq_ignore_ascii_case(t))))
        .map(|s| format!("{}\t{}\t{}", s.name, target_label(s), s.tags.join(",")))
        .collect()
}

fn target_label(server: &Server) -> String {
    server.jump_destination()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn named(name: &str) -> Server {
        Server {
            name: name.into(),
            host: format!("{name}.example"),
            ..Default::default()
        }
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(parse(&[]), Ok(Command::Tui));
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
        assert_eq!(parse(&args(&["-V"])), Ok(Command::Version));
        assert_eq!(
            parse(&args(&["node-2"])),
            Ok(Command::Connect {
                name: "node-2".into(),
                mode: LaunchMode::Ssh
            })
        );
        assert_eq!(
            parse(&args(&["node-2", "--mosh"])),
            Ok(Command::Connect {
                name: "node-2".into(),
                mode: LaunchMode::Mosh
            })
        );
        assert_eq!(
            parse(&args(&["connect", "ls", "--sftp"])),
            Ok(Command::Connect {
                name: "ls".into(),
                mode: LaunchMode::Sftp
            })
        );
        assert_eq!(parse(&args(&["ls"])), Ok(Command::List { tag: None }));
        assert_eq!(
            parse(&args(&["ls", "--tag", "#prod"])),
            Ok(Command::List {
                tag: Some("prod".into())
            })
        );
        assert_eq!(
            parse(&args(&["cmd", "db"])),
            Ok(Command::Print { name: "db".into() })
        );
        assert_eq!(
            parse(&args(&["import"])),
            Ok(Command::Import { path: None })
        );
        assert_eq!(
            parse(&args(&["import", "/tmp/cfg"])),
            Ok(Command::Import {
                path: Some("/tmp/cfg".into())
            })
        );
    }

    #[test]
    fn rejects_junk() {
        for bad in [
            &["--nope"][..],
            &["node", "--ftp"],
            &["ls", "extra"],
            &["cmd"],
            &["connect"],
            &["--help", "x"],
            &["import", "a", "b"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn finds_by_exact_name_then_unique_prefix() {
        let mut config = Config::default();
        for name in ["node-1", "node-2", "gitea", "node"] {
            config.add(named(name));
        }
        assert_eq!(find_server(&config, "NODE"), Ok(3), "exact beats prefix");
        assert_eq!(find_server(&config, "git"), Ok(2));
        let err = find_server(&config, "node-").unwrap_err();
        assert!(err.contains("node-1") && err.contains("node-2"), "{err}");
        assert!(find_server(&config, "zzz").is_err());
    }

    #[test]
    fn list_rows_are_tab_separated_and_filterable() {
        let mut config = Config::default();
        let mut a = named("a");
        a.username = Some("sam".into());
        a.port = Some(2222);
        a.tags = vec!["prod".into(), "eu".into()];
        config.add(a);
        let mut b = named("b");
        b.pinned = true;
        config.add(b);

        assert_eq!(
            list_rows(&config, None),
            ["b\tb.example\t", "a\tsam@a.example:2222\tprod,eu"]
        );
        assert_eq!(list_rows(&config, Some("PROD")).len(), 1);
        assert!(list_rows(&config, Some("nope")).is_empty());
    }
}
