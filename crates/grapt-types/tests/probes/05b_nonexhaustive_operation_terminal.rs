// Probe 5, terminal clause: a match over the closed OperationTerminal that
// misses a variant must not compile. Adding a terminal fact has to break every
// owner match site, which is the whole point of the vocabulary being closed.
use grapt_types::OperationTerminal;

fn label(terminal: &OperationTerminal) -> &'static str {
    match terminal {
        OperationTerminal::Completed => "completed",
        OperationTerminal::Failed { .. } => "failed",
        OperationTerminal::Cancelled { .. } => "cancelled",
        // `ProcessLoss` is deliberately missing.
    }
}

fn main() {
    let _ = label;
}
