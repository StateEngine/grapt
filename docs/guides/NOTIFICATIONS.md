# Terminal notifications

R sends a short terminal notification when an approval or question arrives.
It also notifies when a turn completes while the terminal is unfocused or
there has been no keyboard or paste input for five seconds. These are event
notifications: becoming idle does not start a timer or replay a past completion.

Notifications are on by default. Pass --no-notifications to disable them.
Some terminals use OSC 9; the others use the bell.
OSC 9 is wrapped for tmux passthrough. Your terminal's notification and bell
settings determine how the alert appears.
