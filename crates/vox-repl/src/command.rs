use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplCommand {
    Help,
    Quit,
    Reset,
    Clear,
    Env,
    Snapshot(String),
    Restore(String),
    TypeOf(String),
    Run(String),
    Handles,
    Show(String),
    Drop(String),
    XOpt(String),
    Session(SessionCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    Connect(String),
    New(Option<String>),
    Reserve,
    List,
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
            ":snapshot" => Ok(Self::Snapshot(parts.collect::<Vec<_>>().join(" "))),
            ":restore" => Ok(Self::Restore(parts.collect::<Vec<_>>().join(" "))),
            ":handles" => Ok(Self::Handles),
            ":type" => Ok(Self::TypeOf(parts.collect::<Vec<_>>().join(" "))),
            ":run" => Ok(Self::Run(parts.collect::<Vec<_>>().join(" "))),
            ":show" => Ok(Self::Show(parts.collect::<Vec<_>>().join(" "))),
            ":drop" => Ok(Self::Drop(parts.collect::<Vec<_>>().join(" "))),
            ":xopt" => Ok(Self::XOpt(parts.collect::<Vec<_>>().join(" "))),
            ":session" => parse_session_command(parts.collect::<Vec<_>>()),
            other => Err(format!("unknown REPL command `{other}`")),
        }
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
        other => Err(format!("unknown `:session` subcommand `{other}`")),
    }
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
}
