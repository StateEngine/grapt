# Reasoning effort

Open the native model picker and choose the model with an offered effort, such
as `[high]`. Model and effort change together. The selection line shows an
explicit effort. `[default effort]` uses the configured default, otherwise the
account's default; it does not mean reasoning is disabled.

For a native seat, use `grapt code --reasoning-effort high [repository]`.
The flag applies to the first model choice or resumed session. Picking an
explicit effort in the picker takes precedence. An unsupported choice is
refused and does not start inference.

Headless runs use `grapt run --service <profile> --reasoning-effort high "task"`.
This also works with `--background`. Use `--reasoning-effort default` to return
to the model's configured/account default. `--thinking` independently controls
whether the CLI prints reasoning; it does not select reasoning effort.

In the profile's `opencode.jsonc`, set `provider.selected_variant` to an offered
level. A connected discovery catalog supplies the choices, so do not duplicate
its levels in `provider.variants`. Configurations without discovery retain their
explicit finite variant catalog. Unsupported configured levels refuse startup.

Codex account discovery supplies its offered levels. Claude discovery uses
`capabilities.effort`; older listings use only exact documented model aliases.
OpenAI API discovery uses exact API-supported aliases from the pinned Codex
catalog when the listing supplies no effort levels. Unknown models keep provider
defaults. Explicit empty offered lists remain empty. Unsupported UI-only modes
such as `ultra` are refused rather than sent as an API effort.

Requests use Responses/Codex `reasoning.effort`, Claude `output_config.effort`,
and compatible Chat Completions `reasoning_effort`. Claude thinking stays governed
by the model's existing thinking capability. Each coding chain keeps its selected
model and effort through tool calls and retries. A later selection applies to new
work; stored selections survive reopening.
