# Measure a clean first request

The kit sends one clean prompt through the normal CLI and Product with the selected
account's tools and instructions. It initializes a new profile and an empty working
directory, then reads the resulting attempt through Product's read-only `usage`
command. Existing profiles and working repositories are not changed.

On Windows, use the Node runtime and binaries from the supplied package:

```powershell
& 'E:\path\package\plugin-runtime\node.exe' 'E:\path\kit\first-request.mjs' `
  --bundle 'E:\path\package' --accounts 'E:\path\account-inputs' `
  --output 'E:\Temp\first-request-sol' --model 'openai/gpt-6.1-sol'
```

`--accounts` names an explicit Grapt account-input directory accepted by
`grapt-server init-profile --from`. `--output` must be a new directory under an
existing parent. Use an empty parent without project instructions for the clean
baseline; inherited instructions are included normally in the measured request.

The output prints provider-observed inclusive input against 6,000 tokens, with
serialized byte counts for tools, instructions, and the remaining input/options.
Bytes are a content-size breakdown, not provider token attribution. The full input
decides the budget, including cache reads and writes; no context is subtracted.
The run directory retains `first-request.json`, `usage.json`, and its isolated
profile. A retry, missing usage, extra attempt, tool request, or missing diagnostic
refuses a clean-request result. Exit 0 means within budget, 3 means over budget,
and 2 means the measurement was refused.

Cache measurement for the Windows row A producer days uses those seats' own
durable attempts: Sol on day 1, Claude on day 2, Luna if those days leave the bar
unproven. Export each session with `grapt-server usage --service <profile>
--session <session-id>`. Include compaction, retries and failures, retain incomplete
coverage explicitly, and reconcile live counters after reconnect/restart. Report
weighted read share, absolute uncached input (inclusive input minus cache reads),
fresh input, cache writes, output and cost. The first-request kit is a separate
budget check; it is not required to launch or measure a producer day. Use the
[seat-day command](CACHE_SEAT_DAY.md) to combine those existing attempts in one
session or UTC-window report.
