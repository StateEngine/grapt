# The exact change list

Everything below is additive and marked in-source with
`ADDED IN THIS VENDORED COPY`. No matching semantics are altered: upstream's own
test suite passes unchanged (525 passed / 0 failed with `--features utf16`,
514 / 0 with default features).

## Added files

| file | what |
|---|---|
| `src/budget.rs` | `Budget` (charge-as-you-go allowance, latching) and `BudgetExhausted` |
| `src/budget_evidence.rs` | tests proving the bound holds on adversarial VALID patterns |
| `PROVENANCE.md`, `PATCH.md` | provenance and this list |

## Charge points

The budget is charged wherever the evaluator performs work, not at one loop:

| site | file | charged per |
|---|---|---|
| instruction dispatch (`'nextinsn`) | `classicalbacktrack.rs` | instruction — also covers **lookaround recursion**, which re-enters this same loop on the same attempter, and **backreference comparisons**, which are instructions |
| `try_backtrack` | `classicalbacktrack.rs` | backtrack-stack step |
| `run_scm_loop_impl` | `classicalbacktrack.rs` | character examined, in both the min-drive and max-drive loops |
| `compute_max_pos` | `classicalbacktrack.rs` | character examined (lazy non-greedy path) |
| `next_match_with_prefix_search` | `classicalbacktrack.rs` | **scan restart** |
| `next_match_anchored` | `classicalbacktrack.rs` | anchored attempt, so no start-predicate path is uncharged |

`with_scm_loop_impl` and `with_scm_compute_max` gained a `budget` parameter to
reach the scanning helpers, which are associated functions and do not see
`self`.

## Plumbing

- `MatchAttempter` gains a `budget` field, defaulting to `Budget::unlimited()`,
  so every existing entry point behaves exactly as before.
- `exec::MatchProducer` gains `exhausted_budget()` with a `false` default, so
  producers that carry no budget are unaffected.
- `Regex::find_from_ucs2_budgeted(text, start, budget)` returns
  `Result<Option<Match>, BudgetExhausted>`.

## One widened assertion

`try_at_pos` carried `debug_assert!(self.bts.len() == 1)` after the backtrack
stack drains. Stopping on budget legitimately leaves entries on that stack, so
the assertion now also admits `self.budget.is_exhausted()`. This is a debug
assertion only; release behaviour is unchanged.

## Measured costs

Obtained by binary-searching the smallest budget that reaches a verdict:

| case | units |
|---|---|
| `^[0-9]+$` on `12345` | 11 |
| `^(a+)+$` on 40 `a` (matches) | 58 |
| `^(a+)\1+$` on 60 `a` + `b` | 1,435 |
| `zzz` scanned across 100,000 `a` | 300,003 |
| `^(a+)+$` on 18 `a` + `!` | 3,211,319 |
| `^(a+)+$` on 22 `a` + `!` | exceeds 50,000,000 |
| `^(x+x+)+y$` on 30 `x` | exceeds 50,000,000 |

The asymmetry in rows 2 and 5 is the point: the same pattern is trivial when it
matches and exponential when it fails, so "it was fast once" is not evidence of
a bound.

## Upstreaming

The budget is intended for upstream. If `regress` gains an equivalent, delete
this directory and restore the registry dependency.
