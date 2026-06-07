use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Submitted(String),
    Cancelled,
}

pub fn edit_chunk(label: &str, initial: &str) -> Result<EditOutcome, String> {
    match configured_editor() {
        EditorMode::Builtin => builtin_editor(label, initial),
        EditorMode::External(command) => external_editor(&command, initial),
    }
}

pub fn view_text(label: &str, text: &str) -> Result<bool, String> {
    let command = match env::var("VOX_VIEWER") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(false),
    };

    let path = temp_editor_path(label, "txt");
    fs::write(&path, text).map_err(|error| error.to_string())?;
    let status = Command::new("/bin/sh")
        .arg("-lc")
        .arg("eval \"$1 \\\"$2\\\"\"")
        .arg("vox-viewer")
        .arg(command)
        .arg(path.as_os_str())
        .status()
        .map_err(|error| error.to_string())?;
    let _ = fs::remove_file(&path);
    if status.success() {
        Ok(true)
    } else {
        Err("viewer exited without opening the dump".to_owned())
    }
}

enum EditorMode {
    Builtin,
    External(String),
}

fn configured_editor() -> EditorMode {
    match env::var("VOX_EDITOR") {
        Ok(value) if value.trim().eq_ignore_ascii_case("builtin") => EditorMode::Builtin,
        Ok(value) if !value.trim().is_empty() => EditorMode::External(value),
        _ => match env::var("EDITOR") {
            Ok(value) if !value.trim().is_empty() => EditorMode::External(value),
            _ => EditorMode::Builtin,
        },
    }
}

fn external_editor(command: &str, initial: &str) -> Result<EditOutcome, String> {
    let path = temp_editor_path("chunk", "vox");
    fs::write(&path, initial).map_err(|error| error.to_string())?;
    let status = Command::new("/bin/sh")
        .arg("-lc")
        .arg("eval \"$1 \\\"$2\\\"\"")
        .arg("vox-editor")
        .arg(command)
        .arg(path.as_os_str())
        .status()
        .map_err(|error| error.to_string())?;
    let result = if status.success() {
        let text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        EditOutcome::Submitted(text)
    } else {
        EditOutcome::Cancelled
    };
    let _ = fs::remove_file(&path);
    Ok(result)
}

fn builtin_editor(label: &str, initial: &str) -> Result<EditOutcome, String> {
    let mut stdout = io::stdout();
    writeln!(
        stdout,
        "Builtin Vox editor for {label}. Finish with `.submit`; cancel with `.cancel`."
    )
    .map_err(|error| error.to_string())?;
    if !initial.trim().is_empty() {
        writeln!(stdout, "Current chunk:").map_err(|error| error.to_string())?;
        writeln!(stdout, "{}", "-".repeat(72)).map_err(|error| error.to_string())?;
        writeln!(stdout, "{initial}").map_err(|error| error.to_string())?;
        writeln!(stdout, "{}", "-".repeat(72)).map_err(|error| error.to_string())?;
        writeln!(stdout, "Enter the full replacement chunk below:")
            .map_err(|error| error.to_string())?;
    }
    stdout.flush().map_err(|error| error.to_string())?;

    let mut buffer = String::new();
    loop {
        write!(stdout, "::: ").map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == ".cancel" {
            return Ok(EditOutcome::Cancelled);
        }
        if trimmed == ".submit" {
            return Ok(EditOutcome::Submitted(buffer));
        }
        buffer.push_str(&line);
    }
}

fn temp_editor_path(label: &str, extension: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let label = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    env::temp_dir().join(format!("vox-repl-{label}-{stamp}.{extension}"))
}
