use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplCommand {
    Help,
    Quit,
    Reset,
    Clear,
    Env,
    Chunk,
    Edit(String),
    Snapshot(String),
    Restore(String),
    TypeOf(String),
    Run(String),
    Mount(Vec<String>),
    Handles,
    Show(String),
    Drop(String),
    Opt(OptCommand),
    Session(SessionCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptCommand {
    Get(Option<String>),
    Set { mode: String, objects: Vec<String> },
    Dump(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    Connect(String),
    New(Option<String>),
    Reserve,
    List,
    Transfer {
        binding: String,
        source: String,
        alias: Option<String>,
    },
}

impl FromStr for ReplCommand {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let line = raw.trim();
        let mut parts = line.split_whitespace();
        let Some(head) = parts.next() else {
            return Err("empty REPL command".to_owned());
        };

        match head {
            ":help" => Ok(Self::Help),
            ":quit" => Ok(Self::Quit),
            ":reset" => Ok(Self::Reset),
            ":clear" => Ok(Self::Clear),
            ":env" => Ok(Self::Env),
            ":chunk" => Ok(Self::Chunk),
            ":edit" => Ok(Self::Edit(parts.collect::<Vec<_>>().join(" "))),
            ":snapshot" => Ok(Self::Snapshot(parts.collect::<Vec<_>>().join(" "))),
            ":restore" => Ok(Self::Restore(parts.collect::<Vec<_>>().join(" "))),
            ":handles" => Ok(Self::Handles),
            ":type" => Ok(Self::TypeOf(parts.collect::<Vec<_>>().join(" "))),
            ":run" => Ok(Self::Run(parts.collect::<Vec<_>>().join(" "))),
            ":mount" => {
                let rest = line[":mount".len()..].trim();
                Ok(Self::Mount(collect_quoted_tokens(rest)))
            }
            ":show" => Ok(Self::Show(parts.collect::<Vec<_>>().join(" "))),
            ":drop" => Ok(Self::Drop(parts.collect::<Vec<_>>().join(" "))),
            ":opt" => parse_opt_command(parts.collect::<Vec<_>>()),
            ":session" => parse_session_command(parts.collect::<Vec<_>>()),
            other => Err(format!("unknown REPL command `{other}`")),
        }
    }
}

fn parse_opt_command(parts: Vec<&str>) -> Result<ReplCommand, String> {
    let Some(action) = parts.first().copied() else {
        return Err("`:opt` requires a subcommand".to_owned());
    };

    match action {
        "get" => {
            let object = parts[1..].join(" ");
            Ok(ReplCommand::Opt(OptCommand::Get(
                (!object.trim().is_empty()).then_some(object),
            )))
        }
        "set" => {
            let Some(mode) = parts.get(1).copied() else {
                return Err("`:opt set` requires a mode".to_owned());
            };
            Ok(ReplCommand::Opt(OptCommand::Set {
                mode: mode.to_owned(),
                objects: parts[2..].iter().map(|part| (*part).to_owned()).collect(),
            }))
        }
        "dump" => {
            let object = parts[1..].join(" ");
            Ok(ReplCommand::Opt(OptCommand::Dump(
                (!object.trim().is_empty()).then_some(object),
            )))
        }
        other => Err(format!("unknown `:opt` subcommand `{other}`")),
    }
}

fn parse_session_command(parts: Vec<&str>) -> Result<ReplCommand, String> {
    let Some(action) = parts.first().copied() else {
        return Err("`:session` requires a subcommand".to_owned());
    };

    match action {
        "connect" => {
            let target = parts[1..].join(" ");
            if target.trim().is_empty() {
                return Err("`:session connect` requires a session id or name".to_owned());
            }
            Ok(ReplCommand::Session(SessionCommand::Connect(target)))
        }
        "new" => {
            let name = parts[1..].join(" ");
            let name = if name.trim().is_empty() {
                None
            } else {
                Some(name)
            };
            Ok(ReplCommand::Session(SessionCommand::New(name)))
        }
        "reserve" => Ok(ReplCommand::Session(SessionCommand::Reserve)),
        "list" => Ok(ReplCommand::Session(SessionCommand::List)),
        "transfer" => parse_session_transfer_command(&parts),
        other => Err(format!("unknown `:session` subcommand `{other}`")),
    }
}

fn parse_session_transfer_command(parts: &[&str]) -> Result<ReplCommand, String> {
    let Some(from_index) = parts.iter().position(|part| *part == "from") else {
        return Err("`:session transfer` expects `<binding> from <session>`".to_owned());
    };
    if from_index == 1 {
        return Err("`:session transfer` requires a binding before `from`".to_owned());
    }
    let binding = parts[1..from_index].join(" ");
    let rest = &parts[from_index + 1..];
    if rest.is_empty() {
        return Err("`:session transfer` requires a source session after `from`".to_owned());
    }

    let (source, alias) = if let Some(as_index) = rest.iter().position(|part| *part == "as") {
        if as_index == 0 {
            return Err("`:session transfer` requires a source session before `as`".to_owned());
        }
        if as_index + 1 >= rest.len() {
            return Err("`:session transfer` requires a target name after `as`".to_owned());
        }
        (
            rest[..as_index].join(" "),
            Some(rest[as_index + 1..].join(" ")),
        )
    } else {
        (rest.join(" "), None)
    };

    Ok(ReplCommand::Session(SessionCommand::Transfer {
        binding,
        source,
        alias,
    }))
}

fn collect_quoted_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        let mut token = String::new();
        if ch == '"' {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '"' {
                    break;
                }
                token.push(ch);
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                token.push(ch);
                chars.next();
            }
        }
        if !token.is_empty() {
            tokens.push(token);
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::ReplCommand;
    use std::str::FromStr;

    #[test]
    fn parses_type_command() {
        let command = ReplCommand::from_str(":type input.(image.blur)(radius = 1.0)")
            .expect("command should parse");
        assert!(matches!(command, ReplCommand::TypeOf(expr) if expr.contains("image.blur")));
    }

    #[test]
    fn parses_mount_unquoted_paths() {
        let command = ReplCommand::from_str(":mount /tmp/foo /tmp/bar")
            .expect("command should parse");
        assert_eq!(
            command,
            ReplCommand::Mount(vec!["/tmp/foo".to_owned(), "/tmp/bar".to_owned()])
        );
    }

    #[test]
    fn parses_mount_quoted_path() {
        let command = ReplCommand::from_str(":mount \"/path/with spaces/lib.voxlib\"")
            .expect("command should parse");
        assert_eq!(
            command,
            ReplCommand::Mount(vec!["/path/with spaces/lib.voxlib".to_owned()])
        );
    }

    #[test]
    fn parses_mount_mixed() {
        let command = ReplCommand::from_str(":mount /simple \"/quoted path\" /another")
            .expect("command should parse");
        assert_eq!(
            command,
            ReplCommand::Mount(vec![
                "/simple".to_owned(),
                "/quoted path".to_owned(),
                "/another".to_owned(),
            ])
        );
    }

    #[test]
    fn parses_mount_empty() {
        let command = ReplCommand::from_str(":mount")
            .expect("command should parse");
        assert_eq!(command, ReplCommand::Mount(vec![]));
    }
}
