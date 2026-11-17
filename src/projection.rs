//! The **projection**: what financials-api returns from `POST /projections`.
//!
//! These types mirror `internal/projections/projection.go` → `Projection` and
//! friends. Money is integer cents and rates are basis points, as in the plan.
//!
//! # Strict in, lenient out
//!
//! [`crate::plan::Plan`] rejects unknown fields, because plans come from people
//! and models, where a typo must not be silently ignored. Projections come from
//! our own API, so these types *ignore* unknown fields (serde's default): if a
//! newer financials-api adds a field, this server keeps working. Same library,
//! opposite policy, for a reason.

use serde::{Deserialize, Serialize};

/// One projection result: totals for every year, plus per-account detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Projection {
    /// Number of projected years (the `totals` list also has year 0).
    pub years: i32,
    pub saving_years: i32,
    /// Years between the first and the last person's retirement.
    pub transition_years: i32,
    pub drawdown_years: i32,
    pub currency: String,
    /// Present when the plan has a mortgage.
    #[serde(default)]
    pub mortgage: Option<MortgageProjection>,
    /// Spending per year (phase projections only).
    #[serde(default)]
    pub spending: Vec<YearlySpending>,
    /// Each account's own year-by-year balances.
    pub items: Vec<ProjectedItem>,
    /// Household totals per year, starting with year 0 (`Phase::Starting`).
    pub totals: Vec<YearlyBalance>,
}

/// Which part of the plan a year belongs to.
///
/// This enum's variants carry **no data**, unlike [`crate::Error`]'s: it's a
/// closed set of names, like a Go `const` block of typed strings, but the
/// compiler knows every possible value and `match` must handle each one.
/// `rename_all = "lowercase"` maps `Saving` to and from the JSON string
/// `"saving"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Year 0: the starting balances, before anything happens.
    Starting,
    Saving,
    /// Some people have retired, others haven't.
    Transition,
    Drawdown,
    /// Any phase a newer financials-api might add. `#[serde(other)]` makes
    /// unknown strings land here instead of failing the whole response.
    #[serde(other)]
    Unknown,
}

/// Household totals for one year.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YearlyBalance {
    pub year: i32,
    pub phase: Phase,
    /// End-of-year balance across all accounts.
    pub balance_cents: i64,
    pub contribution_cents: i64,
    pub withdrawal_cents: i64,
    pub growth_cents: i64,
    /// The part of the year's planned spending the accounts couldn't cover.
    /// Greater than 0 means the plan fell short that year.
    pub unfunded_withdrawal_cents: i64,
    /// Balance in accounts that couldn't be withdrawn from yet that year.
    #[serde(default)]
    pub locked_balance_cents: i64,
}

/// One account's projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedItem {
    pub id: String,
    pub name: String,
    pub starting_amount_cents: i64,
    pub annual_return_rate_basis_points: i32,
    #[serde(default)]
    pub drawdown_annual_return_rate_basis_points: Option<i32>,
    pub annual_contribution_cents: i64,
    pub inflate_annual_contribution: bool,
    #[serde(default)]
    pub owner_id: String,
    #[serde(default)]
    pub withdrawal_priority: i32,
    #[serde(default)]
    pub withdrawal_available_after_year: Option<i32>,
    pub yearly_balances: Vec<YearlyBalance>,
}

/// The mortgage's amortization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MortgageProjection {
    /// The year the balance reaches 0, or **0 if it isn't paid off** within
    /// the projection. (The summary turns that 0 into `None`.)
    pub payoff_year: i32,
    pub years: Vec<MortgageYear>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MortgageYear {
    pub year: i32,
    pub balance_cents: i64,
    pub payment_cents: i64,
    pub interest_cents: i64,
    pub paid_from_savings: bool,
}

/// What the household spent in one year.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YearlySpending {
    pub year: i32,
    pub expenses_cents: i64,
    pub mortgage_payment_cents: i64,
    /// Per-expense breakdown when expenses are itemized.
    #[serde(default)]
    pub expenses: Vec<ExpenseAmount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseAmount {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub amount_cents: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_parse_from_lowercase_strings() {
        let phases: Vec<Phase> =
            serde_json::from_str(r#"["starting","saving","transition","drawdown"]"#).unwrap();
        assert_eq!(
            phases,
            [
                Phase::Starting,
                Phase::Saving,
                Phase::Transition,
                Phase::Drawdown
            ]
        );
    }

    #[test]
    fn unknown_phases_and_fields_are_tolerated() {
        let json = r#"{
            "year": 3, "phase": "sabbatical", "balanceCents": 100,
            "contributionCents": 0, "withdrawalCents": 0, "growthCents": 0,
            "unfundedWithdrawalCents": 0, "someNewField": true
        }"#;
        let year: YearlyBalance = serde_json::from_str(json).expect("lenient parsing");
        assert_eq!(year.phase, Phase::Unknown);
        assert_eq!(year.locked_balance_cents, 0, "omitted when zero by the API");
    }
}
