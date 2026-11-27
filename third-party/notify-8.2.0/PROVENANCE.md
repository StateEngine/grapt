# notify 8.2.0

Source: notify-rs/notify, notify/ at a1d7c2d8f80786679d58ec6d5986a1d4278bc8cf,
from the published notify 8.2.0 crate. CC0-1.0 is retained in LICENSE-CC0,
under an approved ruling.

Local changes: report unsupported non-Unicode FSEvents paths through the
existing error callback rather than panic. Native registration behavior and
FFI stay owned by this SDK; the product invalidates catalogs after stream
registration changes.
