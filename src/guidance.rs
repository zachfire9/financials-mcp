//! **Guidance for the model**: the server instructions sent at `initialize`.
//!
//! Tool descriptions and these instructions are part of the program: they're
//! all the model knows about how to use the tools well. They live in one
//! place so they can be reviewed as a set, and the tests below check that
//! every rule from the plan's "Guidance for the model" is present.
//!
//! Each tool's own description is its `///` doc comment in `server.rs`;
//! these instructions cover what applies across tools.

// `concat!` joins string *literals* into one `&'static str` at compile time,
// so the instructions cost nothing at runtime and can't fail to build. It
// only accepts literals (not `const`s or variables), which is why each
// sentence is written out here. One sentence per line keeps diffs readable.

/// Sent to the client in the `initialize` response.
pub const INSTRUCTIONS: &str = concat!(
    "Retirement projection tools backed by the financials-api engine.\n",
    "\n",
    "Numbers: use these tools for every number you report. Never estimate, ",
    "interpolate or calculate a projection yourself; if you need a number the ",
    "tools haven't given you, run another tool call.\n",
    "\n",
    "Units: a plan is the financials-api projection request, with money in integer ",
    "cents and rates in basis points (700 = 7%). Results are in whole dollars.\n",
    "\n",
    "Goals first: before exploring options, ask what the user is optimizing for ",
    "(retiring earliest, spending the most, keeping the biggest buffer, leaving ",
    "money behind) unless they've said. Then present the trade-offs between ",
    "options rather than declaring one plan best: the choice is theirs, and this ",
    "isn't financial advice.\n",
    "\n",
    "Choosing tools (prefer one batch call over many run_projection calls):\n",
    "- run_projection: one plan, optionally with overrides for a single what-if.\n",
    "- compare_scenarios: several named what-ifs side by side, up to 12 per call.\n",
    "- sweep: where a plan stops working as one input changes (spending, ",
    "retirement timing, returns, expense scale).\n",
    "- find_max_spending: the most the plan can spend, optionally keeping a ",
    "minimum balance. Don't search for it by hand.\n",
    "- withdrawal_order_options: which accounts to draw from first.\n",
    "\n",
    "Assumptions and limitations: every result includes the assumptions it used; ",
    "state them. Always mention that projections use a fixed return every year ",
    "(no market downturns or sequence-of-returns risk), include no taxes, and ",
    "include no Social Security or pensions unless the plan's inputs do. Results ",
    "are projections, not guarantees.\n",
    "\n",
    "Errors: a tool error says what was wrong and how to fix it. Correct the call ",
    "and try again; never fill in a result yourself.\n",
    "\n",
    "Privacy: plans hold private financial data. Use only the details the ",
    "conversation needs.",
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Each rule from the plan's "Guidance for the model", with a phrase that
    /// must appear (case-insensitive). If the wording changes, the test makes
    /// someone decide whether the rule is still there.
    const REQUIRED: &[(&str, &str)] = &[
        (
            "every number comes from tools",
            "use these tools for every number",
        ),
        ("never estimate", "never estimate"),
        ("units", "integer cents"),
        ("ask about goals", "ask what the user is optimizing for"),
        ("present trade-offs", "trade-offs"),
        ("not financial advice", "isn't financial advice"),
        ("prefer batch tools", "prefer one batch call"),
        ("state assumptions", "state them"),
        ("fixed returns", "fixed return every year"),
        ("sequence risk", "sequence-of-returns risk"),
        ("no taxes", "no taxes"),
        ("no social security", "social security"),
        ("errors: fix and retry", "correct the call"),
    ];

    #[test]
    fn instructions_mention_every_required_rule() {
        let text = INSTRUCTIONS.to_lowercase();
        // Collect every missing rule so one run reports them all.
        let missing: Vec<&str> = REQUIRED
            .iter()
            .filter(|(_, phrase)| !text.contains(&phrase.to_lowercase()))
            .map(|(rule, _)| *rule)
            .collect();
        assert!(missing.is_empty(), "missing rules: {missing:?}");
    }

    #[test]
    fn instructions_name_every_tool() {
        for tool in [
            "run_projection",
            "compare_scenarios",
            "sweep",
            "find_max_spending",
            "withdrawal_order_options",
        ] {
            assert!(INSTRUCTIONS.contains(tool), "{tool} isn't mentioned");
        }
    }

    #[test]
    fn instructions_stay_short() {
        // The model reads this at the start of every session; keep it a page.
        assert!(INSTRUCTIONS.len() < 2_500, "{} bytes", INSTRUCTIONS.len());
    }

    /// Every limit a tool enforces, triggered once, with the suggestion its
    /// error must carry. A limit without a fix leaves the model guessing.
    #[test]
    fn limit_errors_suggest_a_fix() {
        use crate::compare::Scenario;
        use crate::overrides::{Override, apply_overrides};
        use crate::plan::Plan;
        use crate::sweep::Sweep;
        use serde_json::json;

        let plan =
            Plan::from_json_checked(include_str!("../tests/fixtures/plan-household.json")).unwrap();
        let override_error = |op: serde_json::Value| {
            let ops = Override::list_from_value(json!([op])).unwrap();
            apply_overrides(&plan, &ops).unwrap_err().to_string()
        };
        let named = |names: Vec<String>| {
            Scenario::list_from_value(json!(
                names
                    .iter()
                    .map(|n| json!({ "name": n }))
                    .collect::<Vec<_>>()
            ))
            .unwrap_err()
            .to_string()
        };

        let cases: Vec<(String, &str)> = vec![
            (
                named((0..13).map(|n| format!("S{n}")).collect()),
                "split a longer list into several calls",
            ),
            (
                named(vec!["A".into(), "A".into()]),
                "give each scenario a different name",
            ),
            (named(vec!["x".repeat(81)]), "use a shorter label"),
            (named(vec![" ".into()]), "such as"),
            (
                Override::list_from_value(json!(vec![
                    json!({"op": "set_inflation", "basisPoints": 1});
                    21
                ]))
                .unwrap_err()
                .to_string(),
                "split them across scenarios",
            ),
            (
                override_error(json!({"op": "shift_retirement", "years": 1, "personId": "x"})),
                "use an `id` from the plan's `people`",
            ),
            (
                override_error(json!({"op": "remove_expense", "expenseId": "x"})),
                "use an `id` from the plan's `expenses`",
            ),
            (
                override_error(json!({"op": "set_item", "itemId": "x", "withdrawalPriority": 1})),
                "use an `id` from the plan's `items`",
            ),
            (
                override_error(
                    json!({"op": "add_expense", "expense": {"id": "expense-travel", "name": "T", "annualAmountCents": 1}}),
                ),
                "choose a new id",
            ),
            (
                override_error(json!({"op": "set_expense", "expenseId": "expense-travel"})),
                "besides the id",
            ),
            (
                Sweep::from_value(
                    json!({"parameter": "expense_scale", "values": (0..26).collect::<Vec<i64>>()}),
                )
                .unwrap_err()
                .to_string(),
                "finer values around the breakpoint",
            ),
            (
                Sweep::from_value(json!({"parameter": "expense_scale", "values": [2, 1]}))
                    .unwrap_err()
                    .to_string(),
                "sort them",
            ),
        ];

        for (message, suggestion) in cases {
            assert!(
                message.contains(suggestion),
                "{message:?} should suggest {suggestion:?}"
            );
        }
    }
}
