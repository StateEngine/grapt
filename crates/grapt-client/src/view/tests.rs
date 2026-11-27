//! Projection checks retained independently of whole-screen rendering.
use crate::bounds::Bounds;
use crate::client::Client;
use crate::double::ContractDouble;
use crate::projection::SliceKind;
use crate::wire::Spelled;

mod diff;
mod queued_input;
