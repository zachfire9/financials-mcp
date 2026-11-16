//! The **plan**: the inputs to one retirement projection.
//!
//! These types mirror the financials-api request body for `POST /projections`
//! (`internal/projections/projection.go` → `Request` and friends) field for
//! field, so a plan can be sent to the API unchanged. Required and optional
//! fields follow the planner UI's TypeScript types (`src/financialItemsApi.ts`
//! in financials-ui), so every plan the UI can produce loads here.
//!
//! Money is integer cents (`i64`) and rates are integer basis points (`i32`,
//! 700 = 7%), exactly as in the API. There are no floats anywhere.
//!
//! # How serde turns JSON into these structs
//!
//! `#[derive(Serialize, Deserialize)]` asks the `serde` crate to generate the
//! code that converts each struct to and from JSON at compile time. There is no
//! runtime reflection, unlike Go's `encoding/json` or Python's `json` module.
//! The `#[serde(...)]` attributes play the role of Go's struct tags:
//!
//! | Go tag / behavior                        | serde attribute here                           |
//! |------------------------------------------|------------------------------------------------|
//! | `json:"annualWithdrawalCents"`           | `rename_all = "camelCase"` on the struct       |
//! | missing field decodes to the zero value  | `default` (otherwise the field is required)    |
//! | `omitempty`                              | `skip_serializing_if = "..."`                  |
//! | `*int` (nil means "not set")             | `Option<i32>` (`None` means "not set")         |
//! | `DisallowUnknownFields()` in the API     | `deny_unknown_fields`                          |

use serde::{Deserialize, Serialize};

/// The inputs to one projection: the financials-api `POST /projections` body.
///
/// Rejecting unknown fields matters: the API rejects them too, and silently
/// dropping a misspelled field (say `anualWithdrawalCents`) would make a
/// projection quietly use $0 instead of failing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    /// Simple mode: project this many years. Use either `years` or the
    /// phase fields (`saving_years` / `drawdown_years`), not both.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub years: i32,
    /// Years of saving before retirement (or per person, see `people`).
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub saving_years: i32,
    /// Years of drawing down savings after the last retirement.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub drawdown_years: i32,
    /// First drawdown year's withdrawal. 0 when `expenses` are itemized.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub annual_withdrawal_cents: i64,
    /// How fast withdrawals grow each year (250 = 2.5%).
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub annual_withdrawal_inflation_rate_basis_points: i32,
    /// Household members, each with their own retirement year.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<PersonInput>,
    /// Withdrawal in years when one person has retired and another hasn't.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub gap_annual_withdrawal_cents: i64,
    /// Itemized drawdown spending, used instead of `annual_withdrawal_cents`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expenses: Vec<ExpenseInput>,
    /// An optional mortgage, amortized monthly over the whole projection.
    ///
    /// `Option<T>` is either `Some(value)` or `None`. Rust has no `nil`: a
    /// value that might be missing says so in its type, and the compiler makes
    /// every use handle the `None` case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mortgage: Option<MortgageInput>,
    /// The accounts being projected. This is the only top-level field without
    /// `default`, so a plan with no `items` at all fails to load.
    pub items: Vec<ItemInput>,
}

/// One account: a 401(k), an IRA, a brokerage account, cash, and so on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemInput {
    /// Optional in requests; the UI sends one for saved items.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    pub amount_cents: i64,
    /// A 3-letter code such as `USD`. All items must share one currency.
    pub currency: String,
    pub annual_return_rate_basis_points: i32,
    /// A different return once drawdown starts. `None` keeps the saving rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawdown_annual_return_rate_basis_points: Option<i32>,
    pub annual_contribution_cents: i64,
    pub inflate_annual_contribution: bool,
    /// Which person owns the account (matches a `PersonInput::id`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_id: String,
    /// 1 = withdraw first, 2 = normal (also when 0/omitted), 3 = last.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub withdrawal_priority: i32,
    /// The last year this account can't be withdrawn from.
    ///
    /// This is where `Option` earns its keep: `None` means "use the default"
    /// (when its owner retires), while `Some(0)` means "available from year
    /// 1". A plain `i32` couldn't tell those apart. Go needs a `*int` pointer
    /// for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawal_available_after_year: Option<i32>,
    pub sort_order: i32,
}

/// A household member with their own retirement timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonInput {
    pub id: String,
    pub name: String,
    pub saving_years: i32,
}

/// One itemized expense in drawdown (housing, travel, healthcare, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpenseInput {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    pub annual_amount_cents: i64,
    /// Overrides the plan's inflation rate for this expense only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflation_rate_basis_points: Option<i32>,
    /// First drawdown year of this expense (1 = first year). 0 = from the start.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub from_year: i32,
    /// Last drawdown year of this expense, inclusive. 0 = until the end.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub to_year: i32,
    /// Older way to set `to_year`, still accepted by the API so older plans
    /// load. Prefer `to_year`.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub years: i32,
}

/// A mortgage paid monthly; payments count as withdrawals once drawdown starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MortgageInput {
    pub balance_cents: i64,
    pub annual_rate_basis_points: i32,
    pub monthly_payment_cents: i64,
}

impl Plan {
    /// Parses a plan from JSON text.
    ///
    /// The return type `Result<Plan, serde_json::Error>` is either
    /// `Ok(plan)` or `Err(error)`: Go's `(Plan, error)` pair as one value.
    /// Error handling gets its own step (Step 03); for now the caller gets
    /// serde's error, which names the problem field and its line and column.
    ///
    /// `json: &str` *borrows* the text: the function reads it without taking
    /// ownership, so the caller can keep using it afterwards.
    pub fn from_json(json: &str) -> Result<Plan, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serializes the plan to compact JSON, ready to send to the API.
    ///
    /// `&self` borrows the plan. Serializing a plan of plain numbers and
    /// strings can't fail, so this returns a `String` and not a `Result`.
    /// `expect` documents that assumption and would stop the program with a
    /// clear message if it were ever wrong.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a plan always serializes to JSON")
    }
}

// `skip_serializing_if` calls a function with a *reference* to the field and
// leaves the field out when it returns `true`. That gives Go's `omitempty`
// behavior: zero values aren't sent. `*value` *dereferences* the reference to
// read the number it points to, like `*p` for a pointer in Go.
fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest plan the types accept: one item, everything else omitted.
    const MINIMAL: &str = r#"{
        "items": [{
            "name": "Brokerage",
            "amountCents": 1000000,
            "currency": "USD",
            "annualReturnRateBasisPoints": 600,
            "annualContributionCents": 0,
            "inflateAnnualContribution": false,
            "sortOrder": 0
        }]
    }"#;

    #[test]
    fn missing_optional_fields_get_defaults() {
        let plan = Plan::from_json(MINIMAL).expect("minimal plan should parse");

        assert_eq!(plan.saving_years, 0);
        assert!(plan.people.is_empty());
        assert!(plan.expenses.is_empty());
        assert_eq!(plan.mortgage, None);

        let item = &plan.items[0];
        assert_eq!(item.id, "");
        assert_eq!(item.owner_id, "");
        assert_eq!(item.withdrawal_priority, 0);
        assert_eq!(item.drawdown_annual_return_rate_basis_points, None);
        assert_eq!(item.withdrawal_available_after_year, None);
    }

    #[test]
    fn fields_serialize_as_camel_case_and_zero_values_are_omitted() {
        let plan = Plan::from_json(MINIMAL).unwrap();
        let json = plan.to_json();

        assert!(json.contains("\"amountCents\":1000000"), "{json}");
        assert!(
            json.contains("\"annualReturnRateBasisPoints\":600"),
            "{json}"
        );
        assert!(!json.contains("amount_cents"), "snake_case leaked: {json}");
        // Zero and empty optional fields are left out, like Go's omitempty.
        assert!(!json.contains("savingYears"), "{json}");
        assert!(!json.contains("ownerId"), "{json}");
        assert!(!json.contains("mortgage"), "{json}");
        // Required fields are always sent, even when zero.
        assert!(json.contains("\"sortOrder\":0"), "{json}");
        assert!(
            json.contains("\"inflateAnnualContribution\":false"),
            "{json}"
        );
    }

    #[test]
    fn some_zero_is_kept_but_none_is_omitted() {
        let mut plan = Plan::from_json(MINIMAL).unwrap();
        // `let mut`: bindings are immutable unless declared mutable.
        plan.items[0].withdrawal_available_after_year = Some(0);
        assert!(
            plan.to_json()
                .contains("\"withdrawalAvailableAfterYear\":0"),
            "Some(0) must be sent: it means 'available from year 1'"
        );

        plan.items[0].withdrawal_available_after_year = None;
        assert!(!plan.to_json().contains("withdrawalAvailableAfterYear"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let typo = MINIMAL.replacen(
            "\"items\"",
            "\"anualWithdrawalCents\": 6000000, \"items\"",
            1,
        );

        let err = Plan::from_json(&typo).expect_err("a misspelled field must not be ignored");
        assert!(err.to_string().contains("anualWithdrawalCents"), "{err}");
    }

    #[test]
    fn missing_required_fields_are_rejected_by_name() {
        let no_name = MINIMAL.replacen("\"name\": \"Brokerage\",", "", 1);
        let err = Plan::from_json(&no_name).expect_err("an item needs a name");
        assert!(err.to_string().contains("missing field `name`"), "{err}");

        let err = Plan::from_json("{}").expect_err("a plan needs items");
        assert!(err.to_string().contains("missing field `items`"), "{err}");
    }

    #[test]
    fn wrong_types_are_rejected() {
        let float_money = MINIMAL.replacen("1000000", "10000.50", 1);
        let err = Plan::from_json(&float_money).expect_err("money must be integer cents");
        assert!(err.to_string().contains("invalid type"), "{err}");
    }
}
