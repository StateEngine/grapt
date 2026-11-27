// Probe: the retry witness must be unforgeable outside the crate.
//
// "Retry only before visible output" is enforced by making
// AttemptDisposition::Retry require a NoVisibleOutput, whose only legitimate
// source is StreamValidator::no_visible_output(). If a caller could construct
// the witness directly, an attempt that already streamed text could be retried
// — the exact defect the contract forbids.

fn main() {
    // The tuple field is pub(crate); this must not compile.
    let forged = grapt_provider::NoVisibleOutput(());
    let _ = grapt_provider::AttemptDisposition::retry(
        grapt_provider::ProviderFailureClass::Transport,
        forged,
    );
}
