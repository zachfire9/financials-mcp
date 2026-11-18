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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Limits financials-api enforces, mirrored for fast, clear errors.
/// `const` values are inlined at compile time, like Go's `const`.
const MAX_YEARS: i32 = 75;
const MAX_EXPENSES: usize = 30;

// `///` doc comments on these types and fields are user-facing: schemars
// copies them into the JSON Schema that MCP clients show the model (Step 09).
// Rust explanations go in `//` comments like this one instead.
//
// Rejecting unknown fields matters: the API rejects them too, and silently
// dropping a misspelled field (say `anualWithdrawalCents`) would make a
// projection quietly use $0 instead of failing.

/// The inputs to one retirement projection: the financials-api
/// `POST /projections` request body. Money is integer cents; rates are integer
/// basis points (700 = 7%).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// Itemized drawdown spending, used instead of `annualWithdrawalCents`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expenses: Vec<ExpenseInput>,
    /// An optional mortgage, amortized monthly over the whole projection.
    // `Option<T>` is either `Some(value)` or `None`. Rust has no `nil`: a value
    // that might be missing says so in its type, and the compiler makes every
    // use handle the `None` case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mortgage: Option<MortgageInput>,
    /// The accounts being projected (required, at least one).
    // The only top-level field without `default`, so a plan with no `items`
    // at all fails to load.
    pub items: Vec<ItemInput>,
}

/// One account: a 401(k), an IRA, a brokerage account, cash, and so on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    /// A different return once drawdown starts. Omit to keep the saving rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawdown_annual_return_rate_basis_points: Option<i32>,
    pub annual_contribution_cents: i64,
    pub inflate_annual_contribution: bool,
    /// Which person owns the account (matches an `id` in `people`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_id: String,
    /// 1 = withdraw first, 2 = normal (also when 0/omitted), 3 = last.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub withdrawal_priority: i32,
    /// The last year this account can't be withdrawn from. Omit to use the
    /// default (when its owner retires); 0 means available from year 1.
    // This is where `Option` earns its keep: `None` means "use the default",
    // while `Some(0)` means "available from year 1". A plain `i32` couldn't
    // tell those apart. Go needs a `*int` pointer for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawal_available_after_year: Option<i32>,
    pub sort_order: i32,
}

/// A household member with their own retirement timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonInput {
    pub id: String,
    pub name: String,
    pub saving_years: i32,
}

/// One itemized expense in drawdown (housing, travel, healthcare, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MortgageInput {
    pub balance_cents: i64,
    pub annual_rate_basis_points: i32,
    pub monthly_payment_cents: i64,
}

impl Plan {
    /// Parses a plan from JSON text, without checking its rules.
    ///
    /// Returns `Ok(plan)`, or an [`Error`] naming *where* the problem is
    /// (`items[0].amountCents`) but never the value found there.
    ///
    /// `json: &str` *borrows* the text: the function reads it without taking
    /// ownership, so the caller can keep using it afterwards.
    pub fn from_json(json: &str) -> Result<Plan, Error> {
        let mut deserializer = serde_json::Deserializer::from_str(json);
        // `serde_path_to_error` wraps the deserializer to remember the field
        // path when something fails. `map_err` converts the error type
        // inside a `Result`, leaving an `Ok` untouched.
        let plan: Plan = serde_path_to_error::deserialize(&mut deserializer).map_err(|err| {
            let path = err.path().to_string();
            Error::from_json_error(&path, err.inner())
        })?;
        // `end()` rejects anything after the plan, such as a second object.
        deserializer
            .end()
            .map_err(|err| Error::from_json_error(".", &err))?;
        Ok(plan)
    }

    /// Parses a plan and checks its rules: the usual way to load one.
    ///
    /// This is what the `?` operator is for. Each `?` means "if this is an
    /// `Err`, return it from this function right now; otherwise unwrap the
    /// `Ok` value and carry on." It replaces Go's
    /// `if err != nil { return nil, err }` with one character.
    pub fn from_json_checked(json: &str) -> Result<Plan, Error> {
        let plan = Plan::from_json(json)?;
        plan.check()?;
        Ok(plan)
    }

    /// Quick pre-checks for rules financials-api would reject.
    ///
    /// financials-api is the real validator; this only catches the obvious
    /// problems early, with the same wording the API uses. Rules that need
    /// the projection engine's details (currency codes, rate ranges, mortgage
    /// interest, name lengths) are left to the API.
    ///
    /// Like the API, it collects every problem rather than stopping at the
    /// first. `Ok(())` means "succeeded, nothing to return": `()` is Rust's
    /// empty value, like a function with no return value in Go.
    pub fn check(&self) -> Result<(), Error> {
        // `Vec::new()` is an empty growable list, like `[]string{}` in Go.
        let mut problems: Vec<String> = Vec::new();

        if self.items.is_empty() {
            problems.push("items are required".to_string());
        }

        if self.uses_phase_projection() {
            if self.years != 0 {
                problems
                    .push("years and savingYears/drawdownYears are mutually exclusive".to_string());
            }
            // `!(0..=MAX_YEARS).contains(&x)` checks that x is outside the
            // inclusive range 0 to 75. `..=` includes the end; `..` doesn't.
            if !(0..=MAX_YEARS).contains(&self.saving_years) {
                problems.push(format!("savingYears must be between 0 and {MAX_YEARS}"));
            }
            if !(0..=MAX_YEARS).contains(&self.drawdown_years) {
                problems.push(format!("drawdownYears must be between 0 and {MAX_YEARS}"));
            }
            if !self.people.is_empty() && self.saving_years != 0 {
                problems.push(
                    "savingYears must be 0 when people are provided; set savingYears per person"
                        .to_string(),
                );
            }
            // `.iter().enumerate()` yields `(index, &person)` pairs, like
            // `for index, person := range people` in Go.
            for (index, person) in self.people.iter().enumerate() {
                if !(0..=MAX_YEARS).contains(&person.saving_years) {
                    problems.push(format!(
                        "people[{index}].savingYears must be between 0 and {MAX_YEARS}"
                    ));
                }
            }
            if self.last_retirement_year() + self.drawdown_years < 1 {
                problems.push(
                    "savingYears and drawdownYears must include at least one projected year"
                        .to_string(),
                );
            }
            if self.annual_withdrawal_cents < 0 {
                problems
                    .push("annualWithdrawalCents must be greater than or equal to 0".to_string());
            }
            if self.gap_annual_withdrawal_cents < 0 {
                problems.push(
                    "gapAnnualWithdrawalCents must be greater than or equal to 0".to_string(),
                );
            }
            if !self.expenses.is_empty() && self.annual_withdrawal_cents != 0 {
                problems
                    .push("annualWithdrawalCents must be 0 when expenses are provided".to_string());
            }
            if self.expenses.len() > MAX_EXPENSES {
                problems.push(format!(
                    "expenses can include at most {MAX_EXPENSES} entries"
                ));
            }
            for (index, expense) in self.expenses.iter().enumerate() {
                if expense.annual_amount_cents < 0 {
                    problems.push(format!(
                        "expenses[{index}].annualAmountCents must be greater than or equal to 0"
                    ));
                }
            }
        } else if !(1..=MAX_YEARS).contains(&self.years) {
            problems.push(format!("years must be between 1 and {MAX_YEARS}"));
        }

        for (index, item) in self.items.iter().enumerate() {
            if item.amount_cents < 0 {
                problems.push(format!(
                    "items[{index}].amountCents must be greater than or equal to 0"
                ));
            }
            if item.annual_contribution_cents < 0 {
                problems.push(format!(
                    "items[{index}].annualContributionCents must be greater than or equal to 0"
                ));
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            // A struct-like enum variant is built with its field names.
            // `problems` is shorthand for `problems: problems`.
            Err(Error::InvalidPlan { problems })
        }
    }

    /// Whether this plan uses saving/drawdown phases rather than simple
    /// `years`. Mirrors the API's `usesPhaseProjection`: any phase field set.
    fn uses_phase_projection(&self) -> bool {
        self.saving_years != 0
            || self.drawdown_years != 0
            || self.annual_withdrawal_cents != 0
            || self.annual_withdrawal_inflation_rate_basis_points != 0
            || !self.people.is_empty()
            || self.gap_annual_withdrawal_cents != 0
            || !self.expenses.is_empty()
            || self.mortgage.is_some()
    }

    /// The year the last person retires (or `saving_years` without people).
    ///
    /// An iterator chain: `.map` turns each person into their saving years,
    /// `.max()` picks the largest and returns an `Option` (there may be no
    /// people), and `.unwrap_or` supplies the value to use when it's `None`.
    fn last_retirement_year(&self) -> i32 {
        self.people
            .iter()
            .map(|person| person.saving_years)
            .max()
            .unwrap_or(self.saving_years)
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
        assert!(
            err.to_string()
                .contains("unknown field `anualWithdrawalCents`"),
            "{err}"
        );
    }

    #[test]
    fn missing_required_fields_are_rejected_by_name() {
        let no_name = MINIMAL.replacen("\"name\": \"Brokerage\",", "", 1);
        let err = Plan::from_json(&no_name).expect_err("an item needs a name");
        assert_eq!(
            err.to_string(),
            "plan has a problem at `items[0]`: missing field `name`"
        );

        let err = Plan::from_json("{}").expect_err("a plan needs items");
        assert_eq!(
            err.to_string(),
            "plan has a problem at the top level: missing field `items`"
        );
    }

    #[test]
    fn wrong_types_are_rejected() {
        let float_money = MINIMAL.replacen("1000000", "10000.50", 1);
        let err = Plan::from_json(&float_money).expect_err("money must be integer cents");
        assert_eq!(
            err.to_string(),
            "plan has a problem at `items[0].amountCents`: invalid type, expected a whole number (cents or basis points, no decimals)"
        );
        // The private value never appears in the error.
        assert!(!err.to_string().contains("10000"), "{err}");
    }

    // ---- check(): quick pre-checks --------------------------------------

    /// Parses `MINIMAL` and lets a test change it before checking. `impl
    /// FnOnce(&mut Plan)` means "any closure that can be called once with a
    /// mutable plan"; closures are written `|plan| { ... }`.
    fn minimal_with(change: impl FnOnce(&mut Plan)) -> Plan {
        let mut plan = Plan::from_json(MINIMAL).unwrap();
        plan.years = 30;
        change(&mut plan);
        plan
    }

    /// The problems a plan's check reports, or an empty list if it passes.
    fn problems(plan: &Plan) -> Vec<String> {
        match plan.check() {
            Ok(()) => Vec::new(),
            Err(Error::InvalidPlan { problems }) => problems,
            Err(other) => panic!("unexpected error kind: {other}"),
        }
    }

    #[test]
    fn a_valid_simple_plan_passes() {
        assert_eq!(problems(&minimal_with(|_| {})), Vec::<String>::new());
    }

    #[test]
    fn simple_years_must_be_between_1_and_75() {
        for years in [0, 76, -1] {
            let plan = minimal_with(|plan| plan.years = years);
            assert_eq!(
                problems(&plan),
                ["years must be between 1 and 75"],
                "years={years}"
            );
        }
    }

    #[test]
    fn years_and_phase_fields_are_mutually_exclusive() {
        let plan = minimal_with(|plan| plan.saving_years = 10);
        assert_eq!(
            problems(&plan),
            ["years and savingYears/drawdownYears are mutually exclusive"]
        );
    }

    #[test]
    fn items_are_required() {
        let plan = minimal_with(|plan| plan.items.clear());
        assert_eq!(problems(&plan), ["items are required"]);
    }

    #[test]
    fn negative_amounts_are_rejected_with_the_field_path() {
        let plan = minimal_with(|plan| {
            plan.items[0].amount_cents = -5;
            plan.items[0].annual_contribution_cents = -1;
        });
        assert_eq!(
            problems(&plan),
            [
                "items[0].amountCents must be greater than or equal to 0",
                "items[0].annualContributionCents must be greater than or equal to 0",
            ]
        );
    }

    #[test]
    fn phase_plan_rules() {
        let plan = minimal_with(|plan| {
            plan.years = 0;
            plan.saving_years = 80;
            plan.drawdown_years = 30;
            plan.annual_withdrawal_cents = 6_000_000;
            plan.expenses = (0..31)
                .map(|n| ExpenseInput {
                    id: String::new(),
                    name: format!("Expense {n}"),
                    annual_amount_cents: 100,
                    inflation_rate_basis_points: None,
                    from_year: 0,
                    to_year: 0,
                    years: 0,
                })
                .collect();
        });
        assert_eq!(
            problems(&plan),
            [
                "savingYears must be between 0 and 75",
                "annualWithdrawalCents must be 0 when expenses are provided",
                "expenses can include at most 30 entries",
            ]
        );
    }

    #[test]
    fn people_set_saving_years_per_person() {
        let plan = minimal_with(|plan| {
            plan.years = 0;
            plan.saving_years = 10;
            plan.drawdown_years = 20;
            plan.annual_withdrawal_cents = 1;
            plan.people = vec![PersonInput {
                id: "p1".into(),
                name: "Person".into(),
                saving_years: 90,
            }];
        });
        assert_eq!(
            problems(&plan),
            [
                "savingYears must be 0 when people are provided; set savingYears per person",
                "people[0].savingYears must be between 0 and 75",
            ]
        );
    }

    #[test]
    fn a_phase_plan_needs_at_least_one_year() {
        let plan = minimal_with(|plan| {
            plan.years = 0;
            plan.annual_withdrawal_inflation_rate_basis_points = 250;
        });
        assert_eq!(
            problems(&plan),
            ["savingYears and drawdownYears must include at least one projected year"]
        );
    }

    #[test]
    fn check_messages_never_include_amounts() {
        let plan = minimal_with(|plan| plan.items[0].amount_cents = -987_654);
        let message = plan.check().unwrap_err().to_string();
        assert!(!message.contains("987"), "{message}");
    }

    // ---- from_json / from_json_checked ----------------------------------

    #[test]
    fn malformed_json_reports_line_and_column_only() {
        let err = Plan::from_json("{\n  \"items\": [\n").unwrap_err();
        assert!(matches!(err, Error::InvalidJson { line: 3, .. }), "{err:?}");

        let err = Plan::from_json("{\"items\": []} trailing").unwrap_err();
        assert!(matches!(err, Error::InvalidJson { .. }), "{err:?}");
    }

    #[test]
    fn from_json_checked_parses_then_checks() {
        // Parses fine but fails a rule: `?` passes the check error through.
        let err = Plan::from_json_checked(MINIMAL).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid plan: years must be between 1 and 75"
        );

        // A parse error stops before the check.
        let err = Plan::from_json_checked("not json").unwrap_err();
        assert!(matches!(err, Error::InvalidJson { .. }), "{err:?}");
    }
}
