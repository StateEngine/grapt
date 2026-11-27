# Headless seat acceptance without provider spend

Run the real provisioning CLI and headless client against disposable model,
MCP and native-mail peers. Use an ordinary `grapt` CLI and a separately staged
`grapt-server` built with `grapt-product/loopback-provider-testkit`. The driver
refuses an unlabelled release product. Keep testkit binaries outside installations.

```sh
python3 scripts/stateengine-native/headless_acceptance.py \
  --cli /absolute/test-binaries/grapt \
  --product /absolute/test-binaries/grapt-server \
  --scratch /absolute/new-evidence-directory
```

The scratch directory must not exist. The driver creates fresh profiles and empty
home/config sources, uses dummy credentials, and refuses to share an occupied
loopback mail port 4877. It starts no StateEngine process and contacts no real
provider. Run it on a host where that port is free; do not stop an existing listener.
Each product process must exit successfully through its control pipe before its
case can pass. Failed cases retain their scratch data and leave `complete: false`.

The six cases check:

- Verified packet delivery through the shipped provisioning command, retained
  bytes across headless reattachments, and refusal after service restart until
  capability rebinding and packet redelivery.
- Corrupt packet bytes, missing packet and mismatched acknowledgement, each with
  its specific history diagnostic and zero model requests.
- An idle native-mail wake without another CLI prompt, full message read and
  reply through the published MCP tools, quiet-mail exclusion, delivery receipts,
  duplicate-advisory suppression and subscription shutdown.
- The same mail journey directly after provisioning, with no earlier CLI prompt.

Use `--case ready`, `corrupt`, `missing`, `bad-ack`, `mail` or `mail-startup` to isolate a failure.
`results.json` reports completed cases; per-case CLI output, product logs, public
history, fixture operations and captured model requests distinguish refusal,
missing context, tool failure and shutdown failure. All model usage is synthetic.

These are process integration checks with fixture peers. They do not establish a
real StateEngine capability/bell witness, release-package eligibility, Windows
producer-seat acceptance, provider cache performance or a full seat-day.

Check the driver's own refusal paths with:

```sh
python3 -m unittest discover -s scripts/stateengine-native -p test_headless_acceptance.py
```
