use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplCommand {
    Help,
    Quit,
    Reset,
    List,
    TypeOf(String),
    Purity(String),
    Load(String),
    Reload,
    Run(Vec<String>),
    Handles,
    Show(String),
    Drop(String),
    XOpt(String),
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
            ":list" => Ok(Self::List),
            ":reload" => Ok(Self::Reload),
            ":handles" => Ok(Self::Handles),
            ":type" => Ok(Self::TypeOf(parts.collect::<Vec<_>>().join(" "))),
            ":purity" => Ok(Self::Purity(parts.collect::<Vec<_>>().join(" "))),
            ":load" => Ok(Self::Load(parts.collect::<Vec<_>>().join(" "))),
            ":run" => Ok(Self::Run(parts.map(str::to_owned).collect())),
            ":show" => Ok(Self::Show(parts.collect::<Vec<_>>().join(" "))),
            ":drop" => Ok(Self::Drop(parts.collect::<Vec<_>>().join(" "))),
            ":xopt" => Ok(Self::XOpt(parts.collect::<Vec<_>>().join(" "))),
            other => Err(format!("unknown REPL command `{other}`")),
        }
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
