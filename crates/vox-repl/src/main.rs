use rustyline::DefaultEditor;
use vox_repl::{ReplOutput, ReplSession};

fn main() -> rustyline::Result<()> {
    let mut editor = DefaultEditor::new()?;
    let mut session = ReplSession::default();

    loop {
        match editor.readline(">>> ") {
            Ok(line) => {
                editor.add_history_entry(line.as_str())?;

                match session.handle_line(&line) {
                    ReplOutput::Message(message) => {
                        if !message.is_empty() {
                            println!("{message}");
                        }
                    }
                    ReplOutput::Exit => break,
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(error) => return Err(error),
        }
    }

    Ok(())
}
