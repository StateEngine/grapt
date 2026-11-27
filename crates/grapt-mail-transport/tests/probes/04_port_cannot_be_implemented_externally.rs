// Probe 4: an external crate cannot implement the port.
//
// Closing MailBinding's constructor alone would not have sealed anything: a
// downstream crate could implement RemoteSeatMailTransportV1 itself and return a
// binding from its own `open`. Under ruling J7 the deterministic fake is the
// ONLY v1 implementation and no live adapter may be written, so an
// externally-implementable port is a hole in that ruling rather than an
// extension point.
//
// The trait's supertrait `Sealed` is `pub` inside a PRIVATE module: nameable
// everywhere inside the crate, nowhere outside it. Public consumers may still
// CALL every operation.
//
// # Why all seven methods are implemented here
//
// The obvious probe — `impl RemoteSeatMailTransportV1 for ForeignAdapter {}` —
// PASSES FOR THE WRONG REASON. It emits E0046 (missing trait items) BEFORE the
// E0277 seal error, and E0046 would fire just as loudly if the seal were
// deleted. A probe that cannot tell the sealed case from the unsealed one
// witnesses nothing.
//
// So every method is present with a `todo!()` body: E0046 is then unreachable,
// and the ONLY remaining error is the unsatisfiable `Sealed` bound. Remove the
// supertrait and this file compiles.
//
// One error phase: trait selection (E0277).
use core::future::Future;
use grapt_mail_transport::ack::{DeliveryAckOutcome, DeliveryAdmissionAck};
use grapt_mail_transport::binding::{MailBinding, OpenRequest};
use grapt_mail_transport::cursor::{ArmedMailboxSubscription, MailSnapshot, RemoteMailPage};
use grapt_mail_transport::operation::OperationFailure;
use grapt_mail_transport::identity::ExclusiveSourceCursor;
use grapt_mail_transport::limits::ProviderLimits;
use grapt_mail_transport::port::{
    PageLimits, RemoteSeatMailTransportV1, ResumeRequest, ShutdownBudget, ShutdownReceipt,
};
use grapt_mail_transport::signal::{BulkDismissOutcome, BulkDismissRequest, ChangeSignal};

struct ForeignAdapter;

impl RemoteSeatMailTransportV1 for ForeignAdapter {
    fn open(
        &mut self,
        _request: OpenRequest,
        _limits: ProviderLimits,
    ) -> impl Future<Output = Result<MailBinding, OperationFailure>> + Send {
        core::future::ready(todo!())
    }

    fn subscribe_and_snapshot(
        &mut self,
        _binding: &MailBinding,
        _resume: ResumeRequest,
    ) -> impl Future<Output = Result<ArmedMailboxSubscription, OperationFailure>> + Send {
        core::future::ready(todo!())
    }

    fn fetch_snapshot_page(
        &mut self,
        _binding: &MailBinding,
        _snapshot: &MailSnapshot,
        _after: Option<&ExclusiveSourceCursor>,
        _page: PageLimits,
    ) -> impl Future<Output = Result<RemoteMailPage, OperationFailure>> + Send {
        core::future::ready(todo!())
    }

    fn acknowledge_delivery(
        &mut self,
        _binding: &MailBinding,
        _ack: DeliveryAdmissionAck,
    ) -> impl Future<Output = Result<DeliveryAckOutcome, OperationFailure>> + Send {
        core::future::ready(todo!())
    }

    fn dismiss(
        &mut self,
        _binding: &MailBinding,
        _request: BulkDismissRequest,
    ) -> impl Future<Output = Result<BulkDismissOutcome, OperationFailure>> + Send {
        core::future::ready(todo!())
    }

    fn next_change(
        &mut self,
        _subscription: &mut ArmedMailboxSubscription,
    ) -> impl Future<Output = Result<ChangeSignal, OperationFailure>> + Send + use<> {
        core::future::ready(todo!())
    }

    fn close(
        &mut self,
        _binding: MailBinding,
        _budget: ShutdownBudget,
    ) -> impl Future<Output = Result<ShutdownReceipt, OperationFailure>> + Send {
        core::future::ready(todo!())
    }
}

fn main() {}
