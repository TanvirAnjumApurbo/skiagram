//! Machine-readable output: any report as pretty JSON on stdout.

use std::io::Write;

/// Pretty-print any serializable report (e.g. `Summary`, `ContextReport`) as
/// JSON, followed by a trailing newline.
pub fn print<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}
