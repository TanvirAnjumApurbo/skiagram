//! Machine-readable output: the full `Summary` as pretty JSON on stdout.

use std::io::Write;

use tokscope_core::analysis::aggregate::Summary;

pub fn print(summary: &Summary) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, summary)?;
    writeln!(stdout)?;
    Ok(())
}
