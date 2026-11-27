# Web search, fetch and remote MCP

Web access has three separate paths: provider-hosted search, tools run by Grapt, and tools supplied by a configured MCP server. Enabling one does not enable the others.

## Provider-by-tool matrix

This table describes Grapt's routing, not a guarantee that every model or account accepts every tool. Local and MCP tools still depend on the published catalog and the session's permissions.

| Model provider route | Native hosted `web_search` | Local `web_fetch` | Local/plugin web search | Remote MCP tools |
| --- | --- | --- | --- | --- |
| OpenAI API (Responses) | Available when enabled and not withheld by policy | Built in; subject to `webfetch` permission | Extension-dependent; no built-in local search tool | Available from configured, connected servers; subject to tool permissions |
| ChatGPT/Codex | Available when enabled and not withheld by policy | Built in; subject to `webfetch` permission | Extension-dependent; no built-in local search tool | Available from configured, connected servers; subject to tool permissions |
| Anthropic (Messages) | Not offered by Grapt, even with `web_search` enabled | Built in; subject to `webfetch` permission | Extension-dependent; no built-in local search tool | Available from configured, connected servers; subject to tool permissions |
| OpenAI-compatible (Chat Completions) | Not offered by Grapt | Built in; endpoint must support tool calling | Extension-dependent; endpoint must support tool calling | Configured servers only; endpoint must support tool calling |

The native-search restriction is specific to Grapt's current adapters; it is not a claim about every capability offered by Anthropic or another provider. Using Claude does not prevent local fetch or configured MCP tools, but it does not add hosted search to the Anthropic request.

## Native hosted search

Native web search runs at the provider, separately from callable function tools. Enable it in your user configuration or the service profile's `grapt.json` / `grapt.jsonc`:

```json
{ "web_search": "cached" }
```

Choose `cached` for the provider's cached search index, `live` to allow live web access, or `disabled` to turn native search off. Setup enables `live` in new ChatGPT and OpenAI API profiles. Existing profiles without a user-owned setting remain disabled; choose a mode explicitly to enable search in them. Project configuration may narrow live access to cached access, or disable search, but cannot enable or widen it. A session policy that withholds `websearch` also prevents the native tool from being offered. Other provider protocols do not receive this native tool.

The model chooses when to search during an answer. Sources appear as visible links in the answer; the native TUI keeps their complete destinations when wrapping, scrolling or resizing. Supported terminals make these labels clickable. Search actions and sources remain available in session history after reopening the session.

Search runs at the provider. When you switch accounts or models, the conversation keeps the recorded search results and sources.

## Local fetch and extension search

`web_fetch` runs through Grapt's local tool executor, not the model provider's hosted search service. It takes a complete public HTTP(S) URL:

```json
{ "url": "https://example.com/documentation" }
```

It returns text or Markdown and uses the `webfetch` permission action, bound to the exact origin. Cross-origin redirects require a separate call and permission decision; private, local and reserved destinations are refused. Each fetch has a 60-second deadline and a 512 KiB response-body limit. See [Fetching public web pages](WEB_FETCH.md).

There is no built-in local search tool corresponding to hosted `web_search`. The plugin web-search registry starts empty; a plugin must register a search provider. Its query path needs an available provider, either explicitly chosen for the query or selected through the saved/default selection. A disabled selection suppresses that default. Registering a search provider is not itself a promise of a model-callable search tool: the extension must expose a callable tool if the model is to invoke it. An MCP server can also supply search, under its own published tool name and policy.

Setting `"web_search": "disabled"` only disables native hosted search. It does not disable `web_fetch`, plugin search or MCP tools; those have separate permission and configuration controls.

## Remote MCP

Remote MCP is a Grapt connection to a server, independent of the model provider. It is not the provider's hosted search or hosted MCP integration. The server supplies its own catalog; configuring one does not guarantee that it offers search or fetch.

Put the server in user-owned configuration, such as the service profile's `grapt.json` / `grapt.jsonc`:

```json
{
  "mcp": {
    "servers": {
      "docs": {
        "type": "remote",
        "url": "https://mcp.example.com/mcp",
        "oauth": true
      }
    }
  }
}
```

Project-owned declarations cannot initiate remote connections. A declaration with `disabled: true` or `enabled: false` is not contacted. Only connected, usable servers contribute tools; tool permissions can still withhold or refuse them. MCP tool permissions use the published qualified tool name, not the native `websearch` or local `webfetch` action.

MCP credentials are separate from the model account. For the OAuth example, start the product with that profile and sign in explicitly:

```sh
grapt mcp login docs --service /path/to/profile
grapt mcp logout docs --service /path/to/profile
```

Opening a session or calling a tool does not launch browser sign-in. Static headers are an alternative to OAuth, but a static Authorization header cannot be combined with OAuth. The remote transport requires HTTPS, except for literal loopback HTTP addresses; OAuth requires HTTPS. These are MCP rules, not the public-page destination rules used by `web_fetch`. See [Remote MCP and sign-in](REMOTE_MCP.md) for authentication and transport details.
