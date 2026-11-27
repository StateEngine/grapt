# Credentials during long sessions

A running service reloads its configured credential before each model request,
including automatic compaction. Replacing the configured credential file takes
effect on the next request without restarting the service.

For a ChatGPT sign-in, the service keeps the account it started with. It reads
the stored sign-in generation under the profile's renewal lock, repairs its
derived credential file, and renews tokens close to their stored expiry.
Concurrent requests share that owner; concurrent processes use the same file
lock and re-read the generation before submitting a refresh grant. A bounded
renewal already in progress finishes saving its rotated generation even if the
request that started it times out or is canceled.

After an HTTP 401, recovery first looks for an already rotated credential.
ChatGPT can then renew through its sign-in authority. Another model request is
allowed only when the credential actually changed. It uses a new physical
attempt and stays within the existing attempt limit. If the provider rejects
that replacement too, the service stops instead of repeatedly minting tokens.
A replacement must be accepted before another renewal, including renewal near
expiry. An expired replacement stops the request. An HTTP 403 stays a permission
failure.

Account, configured credential version, protocol and placement remain fixed.
A different sign-in account is refused; start a service with the intended
profile to change accounts. No credential recovery retries output that was
already shown.

If the configured source cannot be restored or read safely, the request stops
before model bytes are sent. ChatGPT can repair its derived credential file
from the authoritative sign-in generation. An expired sign-in that cannot be
renewed also stops the request. Restore the source or sign in again before
retrying.
