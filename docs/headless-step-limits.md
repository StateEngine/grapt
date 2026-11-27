# Headless step limits

Limit a headless task's work with `--max-steps`:

```sh
grapt run --service /path/to/profile --max-steps 10 "Investigate the failing build"
```

A Step is one logical model call in task execution. A call can use several tools;
physical retry attempts share that Step. The service stops before opening Step
11 and reports budget exhaustion if the task still needs to continue. Completed
output and tool results remain in the session. Existing runtime limits can stop
work sooner.

The cap belongs to this prompt and its continuation, including steers that join
while it runs. It is stored with the prompt, so background and queued work keep
the bound after restart. A later prompt starts its own allowance. This flag
changes no profile or session default and cannot be combined with `--resident`.
Use an integer from 1 through 4294967295; zero and malformed values are refused
before connecting.

API clients can submit the same bound as `metadata.maxSteps` on a prompt request.
An exact retry must preserve the bound; omitting or changing it is a conflict.
