# Composer

In R, type `@` followed by part of a file name to search the attached Session's
workspace. Use Up/Down to choose a result and Enter or Tab to insert its relative
path. Paths with spaces are quoted. Then continue your message and press Enter
to send it. The agent opens referenced files with its normal tools and permission
prompts.

Escape closes the picker and keeps your draft. It stays closed until the query
changes. Email addresses do not open the picker. Searches follow workspace ignore
rules and omit `.git` metadata and child symlinks. A “Search limited” message means
that narrowing the query can reveal more matches: each search scans at most
10,000 entries to depth 32 and keeps 64 results. The composer accepts up to 64 KiB.

## Assistant questions

When the assistant calls `request_user_input`, its questions open in the input
band. Use Up/Down to move and Space or Enter to choose an option. For a question
that allows several choices, Space toggles each selection. Ctrl+E opens Other;
type your own answer and press Enter to choose it.

Tab and Shift+Tab move between questions. Ctrl+S sends all answers and lets the
assistant continue. Escape declines the questions. A disconnected user leaves
no live question to replay on reconnection. Headless and `run` sessions without
a person return `no_user_available` immediately so the assistant can continue
with an assumption or explain what is blocked.
