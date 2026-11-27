# Fetching public web pages

The `web_fetch` tool takes one complete public HTTP or HTTPS URL:

```json
{ "url": "https://example.com/documentation" }
```

It returns text or Markdown, together with the page URL and content type. HTML is converted to Markdown and active content is removed. Page content is untrusted data. Long results remain available through the `history` tool.

Fetch uses the existing `webfetch` permission action and binds the capability to the exact origin. A redirect to another origin returns that destination for a separate call and permission decision. Local, private and reserved network destinations are refused, including when a hostname resolves to a mixture of public and private addresses.

Each call has a 60-second deadline, a 512 KiB response-body limit and a bounded rendered result. Unsupported encodings, media types and oversized responses produce an error instead of partial content.
