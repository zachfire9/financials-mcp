//! Summaries of real financials-api responses. The two projection fixtures
//! were produced by a local financials-api from the made-up fixture plans.

use financials_mcp::projection::{Phase, Projection};
use financials_mcp::summary::{MortgageSummary, YearAmount, summarize};

fn load(text: &str) -> Projection {
    serde_json::from_str(text).expect("real API response should parse")
}

#[test]
fn basic_projection_runs_short_in_year_36() {
    let projection = load(include_str!("fixtures/projection-basic.json"));
    assert_eq!(
        projection.totals.len(),
        41,
        "year 0 plus 40 projected years"
    );
    assert_eq!(projection.totals[0].phase, Phase::Starting);

    let summary = summarize(&projection).unwrap();
    assert!(!summary.lasts_through_final_year);
    assert_eq!(summary.depletion_year, Some(36));
    assert_eq!(summary.years_short, 5);
    assert_eq!(summary.ending_balance, 0);
    assert_eq!(
        summary.lowest_balance,
        YearAmount {
            year: 36,
            amount: 0
        }
    );
    assert_eq!(summary.total_withdrawn, 2_716_528);
    assert_eq!(summary.total_unfunded, 655_422);
    assert_eq!(summary.locked_when_short, None);
    assert_eq!(summary.mortgage, None);
    assert_eq!(summary.currency, "USD");
}

#[test]
fn household_projection_has_a_transition_and_a_mortgage() {
    let projection = load(include_str!("fixtures/projection-household.json"));

    // Locked balances appear during the transition years, before the
    // household runs short, so they don't count as "locked when short".
    assert!(
        projection
            .totals
            .iter()
            .any(|year| year.phase == Phase::Transition && year.locked_balance_cents > 0)
    );

    let summary = summarize(&projection).unwrap();
    assert_eq!(summary.depletion_year, Some(24));
    assert_eq!(summary.years_short, 18);
    assert_eq!(summary.total_withdrawn, 1_764_275);
    assert_eq!(summary.total_unfunded, 3_185_954);
    assert_eq!(summary.locked_when_short, None);
    assert_eq!(
        (
            summary.phases.saving_years,
            summary.phases.transition_years,
            summary.phases.drawdown_years
        ),
        (8, 3, 30)
    );
    assert_eq!(
        summary.mortgage,
        Some(MortgageSummary {
            paid_off_in_year: Some(19)
        })
    );
}

#[test]
fn summary_is_much_smaller_than_the_projection() {
    let text = include_str!("fixtures/projection-household.json");
    let summary = summarize(&load(text)).unwrap();
    let summary_json = serde_json::to_string(&summary).unwrap();
    assert!(
        summary_json.len() * 50 < text.len(),
        "summary: {} bytes",
        summary_json.len()
    );
}
