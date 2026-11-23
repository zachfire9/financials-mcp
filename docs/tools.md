# Tool reference

The five tools `financials-mcp serve` offers, and the "what if" overrides they share. Every tool is read-only: it runs projections through financials-api and returns compact results. Plan money is integer cents and rates are basis points (700 = 7%); results are whole dollars.

| Tool | Use it for | Limits |
|---|---|---|
| `run_projection` | One plan, optionally with [overrides](#overrides) for a single what-if | 20 overrides |
| [`compare_scenarios`](#comparing-scenarios) | Several named what-ifs side by side, each compared with the baseline | 12 scenarios |
| [`sweep`](#sweeps) | Where a plan stops (or starts) working as one input changes | 25 values |
| [`find_max_spending`](#maximum-spending) | The most the plan can spend each year, optionally keeping a minimum balance | 25 projections, $100 precision |
| [`withdrawal_order_options`](#withdrawal-order) | Which accounts to draw from first | 13 orders |

Every result includes the `assumptions` it used (timeline, inflation, each account's returns). Errors come back as tool results the model can read, name the field or override at fault, suggest a fix, and never quote amounts.

## run_projection

Takes a `plan` and optional `overrides`, and returns a `summary` and its `assumptions`:

- **`summary`:** `lastsThroughFinalYear`, `depletionYear` (the first year spending can't be covered), `yearsShort`, `endingBalance`, `lowestBalance` (`year`, `amount`), `totalWithdrawn`, `totalUnfunded`, `lockedWhenShort` (money that existed but couldn't be withdrawn yet in the first short year), `phases`, `mortgage.paidOffInYear` and `currency`.
- **`assumptions`:** saving and drawdown years (per person when the plan has people), the withdrawal inflation rate, each account's return rates, and the overrides applied.

## Overrides

`run_projection` accepts an optional `overrides` list: small, named changes applied in order to a copy of the plan, so a "what if" never needs a rewritten plan.

```json
{
  "plan": { "...": "..." },
  "overrides": [
    { "op": "shift_retirement", "years": -2 },
    { "op": "adjust_returns", "basisPoints": -100 }
  ]
}
```

| `op` | Fields | Effect |
|---|---|---|
| `shift_retirement` | `years`, optional `personId` | Retire later (+) or earlier (−), everyone or one person. The plan's final year stays fixed, so drawdown absorbs the change. |
| `set_drawdown_years` | `years` | Sets the drawdown length (moves the final year). |
| `set_annual_withdrawal` | `annualWithdrawalCents` | Plans without itemized expenses only. |
| `scale_expenses` | `percent` | Scales all drawdown spending (itemized expenses or the annual withdrawal, plus the gap withdrawal) to `percent`; 90 = 10% less. Not the mortgage. |
| `add_expense` | `expense` | Adds an itemized expense (same shape as the plan's). |
| `remove_expense` | `expenseId` | Removes an itemized expense. |
| `set_expense` | `expenseId` plus any of `annualAmountCents`, `inflationRateBasisPoints`, `fromYear`, `toYear` | Changes one expense. |
| `adjust_returns` | `basisPoints`, optional `drawdownOnly` | Adds to every account's return (−100 = one point lower). |
| `set_item` | `itemId` plus any of `annualContributionCents`, `annualReturnRateBasisPoints`, `drawdownAnnualReturnRateBasisPoints`, `withdrawalPriority` | Changes one account. |
| `set_inflation` | `basisPoints` | Sets the withdrawal inflation rate. |

The result's `assumptions` describe the changed plan and echo the overrides. Errors name the override at fault, such as `overrides[1] (shift_retirement): no person with id ...`, and never quote amounts. At most 20 overrides per call.

## Comparing scenarios

`compare_scenarios` takes the baseline plan and up to 12 named scenarios, each a list of [overrides](#overrides):

```json
{
  "plan": { "...": "..." },
  "scenarios": [
    { "name": "Retire 2 years later", "overrides": [{ "op": "shift_retirement", "years": 2 }] },
    { "name": "Spend 15% less", "overrides": [{ "op": "scale_expenses", "percent": 85 }] }
  ]
}
```

It returns the baseline's summary and assumptions, then one row per scenario in request order: its summary plus `changeFromBaseline` (scenario minus baseline: `depletionYear`, `yearsShort`, `endingBalance`, `lowestBalance`, `totalUnfunded`; positive means later, more or bigger). A scenario whose overrides don't apply, or whose projection fails, gets an `error` row instead and doesn't affect the others. A bad base plan or a failed baseline fails the whole call.

Scenario names must be unique and at most 80 characters. Projections run at most 4 at a time, to be polite to financials-api.

## Sweeps

`sweep` varies one input across 1 to 25 strictly ascending values, in that input's units:

| `parameter` | Values |
|---|---|
| `annual_withdrawal` | First-year withdrawal in cents (plans without itemized expenses) |
| `retirement_shift` | Years to retire later (+) or earlier (−), for everyone; the final year stays fixed |
| `return_adjustment` | Basis points added to every account's return |
| `expense_scale` | Drawdown spending as a percent of the plan's (90 = 10% less) |

```json
{ "plan": { "...": "..." }, "parameter": "retirement_shift", "values": [-4, -2, 0, 1, 2, 4] }
```

It returns one compact point per value (`lastsThroughFinalYear`, `depletionYear`, `yearsShort`, `endingBalance`, `lowestBalance`, `totalUnfunded`) and the `breakpoints`: each neighboring pair of values where the outcome changes, as `{ "lastsAt": 1, "shortAt": 0 }`. The real threshold lies between the two values, and a finer sweep narrows it down. Every value must be valid and every projection must succeed, since a series with gaps could hide a breakpoint.

## Maximum spending

`find_max_spending` answers "what's the most we can spend each year?" by searching rather than guessing. financials-api always gives the same projection for the same plan, so the server can run projections until it finds the answer:

```json
{ "plan": { "...": "..." }, "constraints": { "minBalanceCents": 20000000, "throughYear": 40 } }
```

- **Spending** is `annualWithdrawalCents`, or for itemized plans the sum of the expenses, all scaled by the same factor. The gap-year withdrawal and the mortgage stay as they are.
- **Constraints** (both optional): no unfunded years, and a balance of at least `minBalanceCents` (default 0) in every year from the first retirement through `throughYear` (default the final year). Saving years don't count, since a plan can start below the buffer and grow past it. Years after `throughYear` aren't checked, so the money may still run out later.
- **Search:** check the least spending allowed (the API requires at least 1 cent of withdrawal), then double from the current spending until a projection fails, then binary search to within $100. At most 25 projections.
- **Result:** `outcome` (`found`, `fails_at_zero` or `no_upper_limit`), `annualSpendingCents` (ready for a plan or the `set_annual_withdrawal` override) and `annualSpending` in dollars, `precision`, `projectionsUsed`, the summary at that amount and, for itemized plans, `scaleBasisPoints` and each scaled expense.

## Withdrawal order

`withdrawal_order_options` takes just a plan and compares the orders its accounts could be drawn down in. financials-api withdraws from the lowest `withdrawalPriority` group first (1 = first, 2 = normal, 3 = last), and accounts in the same group share each withdrawal in proportion to their balances. Only the order of the groups matters, so equivalent assignments are compared once.

- **Candidates:** the current order, all accounts together, each account first, each account last and, for up to 3 accounts (there are only 3 priority levels), every strict order. At most 13 distinct orders, which covers every possible order for 3 accounts and the main variants for up to 6. `truncated` says if more existed.
- **Ranking:** lasts through the final year, then latest depletion year, fewest years short, highest ending balance, least unfunded spending. Ties keep the current order first.
- **Each option:** a description in words, `headline` numbers, `changeFromCurrent` and the `withdrawalPriority` per account, ready for the `set_item` override.
- **Limitation:** projections include no taxes, so the ranking reflects returns and access dates only, not tax-driven advice such as "Roth last."

## What the model is told

When a client connects, the server sends instructions ([`src/guidance.rs`](../src/guidance.rs)) that apply across tools:

- **Every number comes from a tool.** Never estimate or interpolate a projection.
- **Ask about goals first** (retire earliest, spend most, biggest buffer, leave money behind), then present trade-offs rather than one "best" plan. This isn't financial advice.
- **Pick the right tool,** and prefer one batch call over many `run_projection` calls.
- **State the assumptions and limitations every time:** a fixed return each year (no downturns or sequence-of-returns risk), no taxes, and no Social Security or pensions unless the plan includes them.
- **On an error, correct the call and retry.** Every limit's error says how to fix it, for example "give between 1 and 12 scenarios; split a longer list into several calls".

Each tool's description comes from its doc comment in `src/server.rs`. Tests check that the instructions cover each rule and that no implementation notes leak into descriptions or schemas.
