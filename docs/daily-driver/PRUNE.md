# Retire old installed builds

Updates keep older packages. When more than four builds are installed, update
prints their eligible count/size and points to the explicit prune command:

```
grapt update --prune
grapt update --prune --prefix <installation>
```

Quit apps and services using this installation first. Prune keeps the active
build and three additional builds, including the revert target. It ranks builds
by the last activation in the existing history, including a revert to an older
build. If a build has no activation record, commit identity is the fallback;
the result says when that fallback was used. Unreadable or malformed history
refuses pruning rather than pretending it is absent.

Prune reports kept and removed builds. Profiles, sessions, stores, the fixed
Windows launcher and the current/revert-target packages are preserved. Update
and revert never prune implicitly; --prune cannot be combined with --from or
--refresh-launcher.

Interrupted deletion leaves an inactive `.prune-<build>` directory. Run the
same explicit prune command again to finish it. A file in use, unknown entry,
link, hard-link alias, changed route or excessive tree stops with an actionable
path; no user process is killed and no link is followed. A completed second
prune changes nothing. A protected build in quarantine refuses cleanup.
