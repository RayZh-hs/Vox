use rustyline::{Editor, history::DefaultHistory};
use vox_repl::{ReplHelper, ReplOutput, ReplSession};

fn main() -> rustyline::Result<()> {
    let mut editor = Editor::<ReplHelper, DefaultHistory>::new()?;
    editor.set_helper(Some(ReplHelper::default()));
    let mut session = ReplSession::default();

    loop {
        if let Some(helper) = editor.helper_mut() {
            helper.set_snapshot(session.completion_snapshot());
        }

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
