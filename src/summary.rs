//! Turns a full projection into the compact **summary** an agent reads.
//!
//! A projection has every account's balance for every year: tens of kilobytes.
//! An agent comparing ten scenarios needs a few honest numbers per scenario:
//! does the money last, when does it run short, what's the lowest point. This
//! module computes them, and nothing else: no rounding of the engine's math,
//! no judgment about whether a plan is "good".
//!
//! Amounts in the summary are **whole dollars** (rounded from cents), because
//! that's how people and models talk about retirement money.

use serde::Serialize;

use crate::error::Error;
use crate::projection::{Phase, Projection};

/// The compact result of one projection.
///
/// It derives only `Serialize`: summaries are produced here and sent out,
/// never read back in.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// True when no year falls short of its planned spending.
    pub lasts_through_final_year: bool,
    /// First year with spending the accounts couldn't cover, if any.
    pub depletion_year: Option<i32>,
    /// How many years fell short. Can be fewer than the years after
    /// `depletion_year` when locked accounts become available later.
    pub years_short: usize,
    /// Balance at the end of the final year.
    pub ending_balance: i64,
    /// The lowest end-of-year balance after year 0 (earliest year on a tie).
    pub lowest_balance: YearAmount,
    /// Total actually withdrawn over the whole projection.
    pub total_withdrawn: i64,
    /// Total planned spending that couldn't be covered.
    pub total_unfunded: i64,
    /// Money that existed but was locked in the first short year, so "ran
    /// short" isn't mistaken for "ran out". `None` when nothing was locked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_when_short: Option<i64>,
    pub phases: Phases,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mortgage: Option<MortgageSummary>,
    pub currency: String,
}

/// The headline numbers of a summary, for tools that return many results at
/// once (sweeps, withdrawal orders). Whole dollars.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Headline {
    pub lasts_through_final_year: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depletion_year: Option<i32>,
    pub years_short: usize,
    pub ending_balance: i64,
    pub lowest_balance: i64,
    pub total_unfunded: i64,
}

// `From` is the standard trait for infallible conversions. Implementing
// `From<&Summary> for Headline` also gives `summary.into()` for free.
impl From<&Summary> for Headline {
    fn from(summary: &Summary) -> Headline {
        Headline {
            lasts_through_final_year: summary.lasts_through_final_year,
            depletion_year: summary.depletion_year,
            years_short: summary.years_short,
            ending_balance: summary.ending_balance,
            lowest_balance: summary.lowest_balance.amount,
            total_unfunded: summary.total_unfunded,
        }
    }
}

/// A year and an amount in that year.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct YearAmount {
    pub year: i32,
    pub amount: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Phases {
    pub saving_years: i32,
    pub transition_years: i32,
    pub drawdown_years: i32,
}

/// Only present when the plan has a mortgage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MortgageSummary {
    /// The payoff year, or `None` if it isn't paid off within the projection.
    ///
    /// Together with `Summary::mortgage` being an `Option`, this nested
    /// `Option` keeps three cases apart: no mortgage (`mortgage: None`), paid
    /// off (`Some(MortgageSummary { paid_off_in_year: Some(19) })`), and not
    /// paid off (`Some(MortgageSummary { paid_off_in_year: None })`).
    pub paid_off_in_year: Option<i32>,
}

/// Summarizes a projection.
///
/// `projection: &Projection` *borrows* the projection: summarizing only reads
/// it, so the caller keeps ownership and can still use it afterwards.
///
/// Returns an error only for a response that breaks the API's contract (no
/// years at all), which would mean a bug, not a bad plan.
pub fn summarize(projection: &Projection) -> Result<Summary, Error> {
    let totals = &projection.totals;

    // `.last()` returns an `Option<&YearlyBalance>`: `None` for an empty list.
    // `ok_or_else` turns that `None` into an error, and `?` returns it.
    let final_year = totals.last().ok_or_else(|| Error::UnexpectedResponse {
        problem: "projection has no years".to_string(),
    })?;

    // Iterator chains: each step takes the items from the previous one.
    // `.iter()` borrows each year in turn; nothing is copied.
    //
    // `.find(...)` stops at the first year matching the closure and returns
    // `Option<&YearlyBalance>`; `.map(|year| year.year)` turns
    // `Some(&year)` into `Some(year.year)` and leaves `None` alone.
    let depletion_year = totals
        .iter()
        .find(|year| year.unfunded_withdrawal_cents > 0)
        .map(|year| year.year);

    let years_short = totals
        .iter()
        .filter(|year| year.unfunded_withdrawal_cents > 0)
        .count();

    // Year 0 is the starting balance, not a point the plan "reaches", so it's
    // skipped unless it's the only year. `min_by_key` returns the *first*
    // smallest element when there's a tie, which gives the earliest year.
    let lowest = totals
        .iter()
        .filter(|year| year.phase != Phase::Starting)
        .min_by_key(|year| year.balance_cents)
        .unwrap_or(final_year);

    // `.map(...).sum()` adds up one field across all years. The type of the
    // total, `i64`, comes from the variable's annotation.
    let total_withdrawn: i64 = totals.iter().map(|year| year.withdrawal_cents).sum();
    let total_unfunded: i64 = totals
        .iter()
        .map(|year| year.unfunded_withdrawal_cents)
        .sum();

    let locked_when_short = totals
        .iter()
        .find(|year| year.unfunded_withdrawal_cents > 0)
        .map(|year| year.locked_balance_cents)
        // `.filter` on an `Option` keeps `Some(x)` only if the closure is
        // true, so `Some(0)` (nothing locked) becomes `None`.
        .filter(|&locked| locked > 0)
        .map(dollars);

    // `.as_ref()` borrows the mortgage inside the `Option` (giving an
    // `Option<&MortgageProjection>`) so `.map` doesn't try to move it out of
    // the projection, which we only borrowed.
    let mortgage = projection
        .mortgage
        .as_ref()
        .map(|mortgage| MortgageSummary {
            paid_off_in_year: (mortgage.payoff_year > 0).then_some(mortgage.payoff_year),
        });

    Ok(Summary {
        lasts_through_final_year: depletion_year.is_none(),
        depletion_year,
        years_short,
        ending_balance: dollars(final_year.balance_cents),
        lowest_balance: YearAmount {
            year: lowest.year,
            amount: dollars(lowest.balance_cents),
        },
        total_withdrawn: dollars(total_withdrawn),
        total_unfunded: dollars(total_unfunded),
        locked_when_short,
        phases: Phases {
            saving_years: projection.saving_years,
            transition_years: projection.transition_years,
            drawdown_years: projection.drawdown_years,
        },
        mortgage,
        currency: projection.currency.clone(),
    })
}

/// Rounds cents to whole dollars, halves away from zero ($0.50 → $1).
///
/// Integer division in Rust truncates toward zero, as in Go (`-7 / 2 == -3`),
/// so 50 cents is added (or subtracted for negatives) before dividing. No
/// floats are involved, so there are no rounding surprises.
pub fn dollars(cents: i64) -> i64 {
    if cents >= 0 {
        (cents + 50) / 100
    } else {
        (cents - 50) / 100
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::{MortgageProjection, YearlyBalance};

    /// Builds a minimal projection from `(phase, balance, withdrawal,
    /// unfunded, locked)` rows **in whole dollars**, one per year starting at
    /// year 0. The helper converts to cents, as the API sends.
    fn projection(rows: &[(Phase, i64, i64, i64, i64)]) -> Projection {
        let totals = rows
            .iter()
            .enumerate()
            // A pattern can unpack a tuple right in the closure's parameters.
            .map(
                |(index, &(phase, balance, withdrawal, unfunded, locked))| YearlyBalance {
                    year: index as i32, // `as` converts between number types
                    phase,
                    balance_cents: balance * 100,
                    contribution_cents: 0,
                    withdrawal_cents: withdrawal * 100,
                    growth_cents: 0,
                    unfunded_withdrawal_cents: unfunded * 100,
                    locked_balance_cents: locked * 100,
                },
            )
            .collect();
        Projection {
            years: rows.len() as i32 - 1,
            saving_years: 1,
            transition_years: 0,
            drawdown_years: rows.len() as i32 - 2,
            currency: "USD".to_string(),
            mortgage: None,
            spending: Vec::new(),
            items: Vec::new(),
            totals,
        }
    }

    use Phase::{Drawdown, Saving, Starting};

    #[test]
    fn a_plan_that_never_runs_short() {
        let summary = summarize(&projection(&[
            (Starting, 100_000, 0, 0, 0),
            (Saving, 110_000, 0, 0, 0),
            (Drawdown, 80_000, 30_000, 0, 0),
            (Drawdown, 55_000, 30_000, 0, 0),
        ]))
        .unwrap();

        assert!(summary.lasts_through_final_year);
        assert_eq!(summary.depletion_year, None);
        assert_eq!(summary.years_short, 0);
        assert_eq!(summary.ending_balance, 55_000);
        assert_eq!(
            summary.lowest_balance,
            YearAmount {
                year: 3,
                amount: 55_000
            }
        );
        assert_eq!(summary.total_withdrawn, 60_000);
        assert_eq!(summary.total_unfunded, 0);
        assert_eq!(summary.locked_when_short, None);
        assert_eq!(summary.mortgage, None);
    }

    #[test]
    fn a_plan_that_runs_short_in_year_1() {
        let summary = summarize(&projection(&[
            (Starting, 5_000, 0, 0, 0),
            (Drawdown, 0, 5_000, 25_000, 0),
            (Drawdown, 0, 0, 30_000, 0),
        ]))
        .unwrap();

        assert!(!summary.lasts_through_final_year);
        assert_eq!(summary.depletion_year, Some(1));
        assert_eq!(summary.years_short, 2);
        assert_eq!(summary.total_unfunded, 55_000);
    }

    #[test]
    fn lowest_balance_ties_report_the_earliest_year() {
        let summary = summarize(&projection(&[
            (Starting, 50, 0, 0, 0),
            (Drawdown, 0, 50, 0, 0),
            (Drawdown, 0, 0, 0, 0),
        ]))
        .unwrap();
        assert_eq!(summary.lowest_balance, YearAmount { year: 1, amount: 0 });
    }

    #[test]
    fn locked_money_in_the_first_short_year_is_reported() {
        let summary = summarize(&projection(&[
            (Starting, 200_000, 0, 0, 0),
            (Drawdown, 150_000, 10_000, 20_000, 150_000),
            (Drawdown, 120_000, 30_000, 0, 0),
        ]))
        .unwrap();
        assert_eq!(summary.depletion_year, Some(1));
        assert_eq!(
            summary.years_short, 1,
            "short once, then the account unlocks"
        );
        assert_eq!(summary.locked_when_short, Some(150_000));
    }

    #[test]
    fn mortgage_paid_off_versus_not_paid_off() {
        let mut paid = projection(&[(Starting, 1, 0, 0, 0), (Saving, 1, 0, 0, 0)]);
        paid.mortgage = Some(MortgageProjection {
            payoff_year: 19,
            years: Vec::new(),
        });
        let summary = summarize(&paid).unwrap();
        assert_eq!(
            summary.mortgage,
            Some(MortgageSummary {
                paid_off_in_year: Some(19)
            })
        );

        let mut unpaid = paid.clone();
        unpaid.mortgage = Some(MortgageProjection {
            payoff_year: 0,
            years: Vec::new(),
        });
        let summary = summarize(&unpaid).unwrap();
        assert_eq!(
            summary.mortgage,
            Some(MortgageSummary {
                paid_off_in_year: None
            })
        );
    }

    #[test]
    fn an_empty_projection_is_an_unexpected_response() {
        let mut empty = projection(&[(Starting, 0, 0, 0, 0)]);
        empty.totals.clear();
        let err = summarize(&empty).unwrap_err();
        assert!(matches!(err, Error::UnexpectedResponse { .. }), "{err}");
    }

    #[test]
    fn dollar_rounding_at_the_edges() {
        let cases = [
            (0, 0),
            (49, 0),
            (50, 1),
            (149, 1),
            (150, 2),
            (-49, 0),
            (-50, -1),
            (12_345_678, 123_457),
        ];
        for (cents, expected) in cases {
            assert_eq!(dollars(cents), expected, "{cents} cents");
        }
    }

    #[test]
    fn summary_serializes_as_camel_case_and_skips_empty_extras() {
        let summary = summarize(&projection(&[
            (Starting, 100, 0, 0, 0),
            (Saving, 100, 0, 0, 0),
        ]))
        .unwrap();
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"lastsThroughFinalYear\":true"), "{json}");
        assert!(
            json.contains("\"depletionYear\":null"),
            "an explicit null is clearer for 'never'"
        );
        assert!(!json.contains("lockedWhenShort"), "{json}");
        assert!(!json.contains("mortgage"), "{json}");
    }
}
