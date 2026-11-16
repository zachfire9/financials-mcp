//! Loads the made-up fixture plans in `tests/fixtures/` through the public
//! `Plan` API, the way a real exported plan would arrive.

use financials_mcp::plan::Plan;

// `include_str!` embeds a file's contents into the test binary at compile
// time, as a `&'static str` (text that lives for the whole program). The path
// is relative to this source file.
const BASIC: &str = include_str!("fixtures/plan-basic.json");
const HOUSEHOLD: &str = include_str!("fixtures/plan-household.json");

#[test]
fn basic_plan_loads_with_expected_values() {
    let plan = Plan::from_json(BASIC).expect("basic fixture should parse");

    assert_eq!(plan.saving_years, 10);
    assert_eq!(plan.drawdown_years, 30);
    assert_eq!(plan.annual_withdrawal_cents, 6_000_000); // `_` separates digits, like Go and Python
    assert_eq!(plan.items.len(), 3);

    let roth = &plan.items[1];
    assert_eq!(roth.name, "Example Roth IRA");
    assert_eq!(roth.amount_cents, 8_000_000);
    assert_eq!(roth.withdrawal_priority, 3);
}

#[test]
fn household_plan_loads_people_expenses_mortgage_and_optional_fields() {
    let plan = Plan::from_json(HOUSEHOLD).expect("household fixture should parse");

    assert_eq!(plan.people.len(), 2);
    assert_eq!(plan.people[1].saving_years, 11);
    assert_eq!(
        plan.annual_withdrawal_cents, 0,
        "itemized plans don't set it"
    );
    assert_eq!(plan.expenses.len(), 3);
    assert_eq!(plan.expenses[1].inflation_rate_basis_points, Some(500));
    assert_eq!(plan.expenses[0].inflation_rate_basis_points, None);
    assert_eq!(
        (plan.expenses[2].from_year, plan.expenses[2].to_year),
        (1, 10)
    );

    // `if let` runs the block only when the pattern matches: here, only when
    // there is a mortgage. It's a compact `match` with one interesting case.
    if let Some(mortgage) = &plan.mortgage {
        assert_eq!(mortgage.monthly_payment_cents, 200_000);
    } else {
        panic!("household fixture has a mortgage");
    }

    // None ("default: when the owner retires") versus Some(0) ("from year 1").
    assert_eq!(plan.items[0].withdrawal_available_after_year, None);
    assert_eq!(plan.items[2].withdrawal_available_after_year, Some(0));
    assert_eq!(
        plan.items[0].drawdown_annual_return_rate_basis_points,
        Some(500)
    );
    assert_eq!(plan.items[0].owner_id, "person-a");
}

#[test]
fn fixtures_round_trip_to_equivalent_json() {
    for (name, text) in [("basic", BASIC), ("household", HOUSEHOLD)] {
        let plan = Plan::from_json(text).unwrap();

        // Compare parsed JSON values rather than strings, so key order and
        // whitespace don't matter: nothing may be added, dropped or renamed.
        let original: serde_json::Value = serde_json::from_str(text).unwrap();
        let reserialized: serde_json::Value = serde_json::from_str(&plan.to_json()).unwrap();
        assert_eq!(
            reserialized, original,
            "{name} fixture changed on round trip"
        );

        // And the round trip is stable: parsing the output gives the same plan.
        assert_eq!(Plan::from_json(&plan.to_json()).unwrap(), plan);
    }
}
