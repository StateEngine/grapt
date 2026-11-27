# Provider error text

When a provider refuses a request with an HTTP 4xx status, the attempt's
failure message states the provider's reason by default:

```text
the provider returned HTTP 400: {"error":{"message":"..."}}
```

The reason is the provider's response body after masking, cut to at most 512
UTF-8 bytes; a cut ends with `…[truncated]`. Masking replaces:

- the current credential, its JSON-escaped spelling and any prefix of it left
  by the read cap;
- the value of any field whose name ends with `authorization`, `x-api-key`,
  `api_key`, `api-key`, `apikey`, `token`, `secret`, `password` or `cookie`, or
  with `_key`, `-key` or `Key`, or is `key`; an object or array value is masked
  whole;
- Bearer tokens, and `Basic` credentials that look encoded, whatever the scheme's case;
- the user information in `scheme://user:pass@host`;
- tokens with a published key prefix (`sk-`, `sk_`, `gsk_`, `xai-`, `AIza`,
  `AKIA`, `eyJ`, `ghp_`, `gho_`, `ghs_`, `github_pat_`, `xoxb-`) at a word
  start and followed by at least eight token characters;
- any run of 32 or more base64 or hex characters that mixes letters and digits.

Masking cannot identify every possible sensitive detail in provider prose.
Server errors (5xx) keep the status-only message. Routes that sign each request
rather than send a key withhold the error body: their rotating secrets are not
known here, so shape-based masking cannot vouch for it.

A system notice beside the failed execution offers a local copy of the masked
text. To opt in, add this line to the profile's `service.conf` and restart the
service:

```ini
provider_error_text=true
```

The notice names `logs/provider-error.txt` under that service profile. It keeps
only the last saved error, replacing the previous record. Treat the file as
private diagnostics.

The flag defaults to false; `true` and `false` are its only accepted values.
Turning it off stops new local saves and leaves the prior local record in place.
On Unix, records are created with mode 0600 inside a mode 0700 logs directory.
Windows inherits the service profile's ACL, as the existing server log does;
this implementation does not claim POSIX permission bits on Windows.

When the flag is on, each physical reader carries one volatile, masked value to
the product observer. A blocking task writes the private file without holding
any Inbox, Session or store lock, then appends one notice through the existing
elected writer. Its fixed product text says `details saved to
logs/provider-error.txt` only after success, or names an unavailable save.
Vendor text never enters that notice.
Retries have separate attempt identities and get at most one notice each.
There is no separate writer thread, queue or attribution registry. A blocked
destination or interrupted body read can leave the prior file as the last saved
error. Saving failures are loud and do not change status, overflow or retry
classification. They never retry the request.

This shares the classifier's existing 16 KiB body bound and response deadline.
The preview is masked before its 512-byte UTF-8 cut; no independent
read can consume a vendor error before classification. The credential remains
opaque: its private redactor returns bounded masked text, never key material.
Valid JSON previews normalize escape spellings before masking. Malformed escaped
content is withheld with an explicit marker because it cannot be decoded safely.
HTTP previews keep the existing 512-byte limit.
A pending file left by a process crash is a named save failure on later captures,
not silently treated as a completed record.

Streaming failures automatically save a redacted terminal diagnosis to the same
last-error file, without the HTTP opt-in. It contains only the event type,
status, error, incomplete details and known usage fields; response output,
content and metadata are excluded. Each record is at most 4096 bytes, so the
pending and final files together retain at most 8192 bytes. A transport failure
without a terminal frame records that the frame is unavailable. The save notice
is fixed product text and never contains the backend frame.

Responses failures preserve the backend's reason even when top-level usage is
null. Completed answers with malformed accounting remain completed; the cache
status and reply footer show a warning and leave usage unreported. The backend failure message
is masked and bounded before it enters the normal error display. No usage is
invented and diagnostic capture never retries a request.
