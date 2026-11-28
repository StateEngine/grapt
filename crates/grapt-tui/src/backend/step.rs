//! The shape of a command run a step at a time: each step leaves the
//! command's next request, or what the command settled.
use grapt_client::{Accepted, ClientError};
#[cfg(test)]
use grapt_client::{Client, Done};

/// What a command's step leaves: its next step, or what it settled.
pub(super) enum Turn<J, O> {
    Step(J, Result<Accepted, ClientError>),
    Settle(O),
}

/// Run a command's steps to the end on the client's own connection, as a
/// test exercises one without the lane.
#[cfg(test)]
pub(super) fn run<J, O, C: grapt_client::Connection>(
    client: &mut Client<C>,
    mut turn: Turn<J, O>,
    advance: impl Fn(J, &mut Client<C>, Result<Done, ClientError>) -> Turn<J, O>,
) -> O {
    loop {
        let (job, mut next) = match turn {
            Turn::Settle(settled) => return settled,
            Turn::Step(job, next) => (job, next),
        };
        let done = loop {
            match next {
                Ok(Accepted::Next(call)) => {
                    let response = client.peer_mut().call(&call.request);
                    next = client.accept(call, response);
                }
                Ok(Accepted::Done(done)) => break Ok(done),
                Err(error) => break Err(error),
            }
        };
        turn = advance(job, client, done);
    }
}
