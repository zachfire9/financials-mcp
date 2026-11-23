//! **Overrides**: small, explicit changes applied to a copy of a plan.
//!
//! A scenario such as "retire 2 years earlier with returns 1% lower" is the
//! base plan plus a list of overrides:
//!
//! ```json
//! [
//!   { "op": "shift_retirement", "years": -2 },
//!   { "op": "adjust_returns", "basisPoints": -100 }
//! ]
//! ```
//!
//! Each override is one variant of the [`Override`] enum, and
//! [`apply_overrides`] applies them in order to a *clone* of the plan. The
//! caller's plan is never changed. Keeping the set of operations small and
//! named means every scenario is easy to check and to describe back to the
//! user, and the model never has to rewrite a whole plan to ask "what if".

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::plan::{ExpenseInput, Plan};

/// The most overrides one scenario may carry. A scenario is a handful of
/// "what ifs", not a rewritten plan.
pub const MAX_OVERRIDES: usize = 20;

/// Largest return adjustment accepted, in basis points (20 percentage points).
const MAX_RETURN_ADJUSTMENT: i32 = 2_000;

/// Largest spending scale accepted, as a percentage (5x).
const MAX_SCALE_PERCENT: i32 = 500;

// An enum whose variants carry data. In Go this would be an interface plus
// one struct per operation and a type switch; in Python, a class per
// operation or a dict with an "op" key. Here an `Override` is exactly one of
// these variants, and `match` must handle every one of them.
//
// The serde attributes choose the JSON shape:
// - `tag = "op"` makes the enum *internally tagged*: the variant's name sits
//   in an `"op"` field next to its own fields, `{"op": "set_inflation",
//   "basisPoints": 300}`. (serde's default would be `{"SetInflation":
//   {"basisPoints": 300}}`.)
// - `rename_all = "snake_case"` turns variant names into `shift_retirement`.
// - `rename_all_fields = "camelCase"` renames the fields *inside* every
//   variant, matching the plan's JSON style.
// - `deny_unknown_fields`, as in the plan: a misspelled field must fail, not
//   be ignored.
//
// `///` comments are model-facing: they become the schema the model reads.

/// One change to a plan. Overrides apply in order to a copy of the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Override {
    /// Retire `years` later (positive) or earlier (negative): everyone, or
    /// only `personId`. The plan's final year stays the same, so drawdown
    /// gets longer when retiring earlier and shorter when retiring later.
    /// Itemized expense years count from the start of drawdown, so they move
    /// with it.
    ShiftRetirement {
        years: i32,
        /// Only this person (an `id` from the plan's `people`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        person_id: Option<String>,
    },

    /// Set the number of drawdown years (changes the plan's final year).
    SetDrawdownYears { years: i32 },

    /// Set the first drawdown year's withdrawal, in cents. Only for plans
    /// without itemized expenses.
    SetAnnualWithdrawal { annual_withdrawal_cents: i64 },

    /// Scale all drawdown spending to `percent` of its current amount
    /// (90 = 10% less, 120 = 20% more): every itemized expense, or the annual
    /// withdrawal when expenses aren't itemized, plus the gap-year withdrawal.
    /// Mortgage payments are not changed.
    ScaleExpenses { percent: i32 },

    /// Add an itemized expense. Give it an `id` so later overrides can
    /// change it.
    AddExpense { expense: ExpenseInput },

    /// Remove an itemized expense by `id`.
    RemoveExpense { expense_id: String },

    /// Change fields of an itemized expense by `id`. Give at least one field.
    SetExpense {
        expense_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annual_amount_cents: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inflation_rate_basis_points: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_year: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_year: Option<i32>,
    },

    /// Add `basisPoints` to every account's return (-100 = 1 percentage
    /// point lower). With `drawdownOnly`, change only the return used once
    /// drawdown starts.
    AdjustReturns {
        basis_points: i32,
        #[serde(default, skip_serializing_if = "is_false")]
        drawdown_only: bool,
    },

    /// Change fields of one account by `id`. Give at least one field.
    SetItem {
        item_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annual_contribution_cents: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annual_return_rate_basis_points: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drawdown_annual_return_rate_basis_points: Option<i32>,
        /// 1 = withdraw first, 2 = normal, 3 = last.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        withdrawal_priority: Option<i32>,
    },

    /// Set how fast withdrawals grow each year (250 = 2.5%).
    SetInflation { basis_points: i32 },
}

impl Override {
    /// The JSON `op` name, for error messages.
    ///
    /// `&'static str` is a string slice that lives for the whole program:
    /// string literals are baked into the binary, so no allocation is needed.
    /// The `{ .. }` patterns match a variant while ignoring its fields.
    pub fn op_name(&self) -> &'static str {
        match self {
            Override::ShiftRetirement { .. } => "shift_retirement",
            Override::SetDrawdownYears { .. } => "set_drawdown_years",
            Override::SetAnnualWithdrawal { .. } => "set_annual_withdrawal",
            Override::ScaleExpenses { .. } => "scale_expenses",
            Override::AddExpense { .. } => "add_expense",
            Override::RemoveExpense { .. } => "remove_expense",
            Override::SetExpense { .. } => "set_expense",
            Override::AdjustReturns { .. } => "adjust_returns",
            Override::SetItem { .. } => "set_item",
            Override::SetInflation { .. } => "set_inflation",
        }
    }

    /// Parses a list of overrides from JSON (as received in a tool call).
    ///
    /// Like [`Plan::from_json`], problems name *where* they are
    /// (`overrides[1].years`) but never the value found there.
    pub fn list_from_value(value: serde_json::Value) -> Result<Vec<Override>, Error> {
        // `serde_json::Value` is itself a deserializer, so it can be turned
        // straight into `Vec<Override>` without going back to text.
        let overrides: Vec<Override> = serde_path_to_error::deserialize(value).map_err(|err| {
            let path = err.path().to_string();
            // Reuse the plan's value-free error mapping, then re-label it:
            // a `match` that unpacks one variant's fields and builds another.
            match Error::from_json_error(&path, err.inner()) {
                Error::PlanShape { problem, .. } => Error::OverrideShape {
                    // `path` is like `[1].years`, or `.` for the list itself.
                    location: format!("overrides{}", path.trim_start_matches('.')),
                    problem,
                },
                other => other,
            }
        })?;
        if overrides.len() > MAX_OVERRIDES {
            return Err(Error::OverrideShape {
                location: "overrides".to_string(),
                problem: format!("at most {MAX_OVERRIDES} overrides are allowed"),
            });
        }
        Ok(overrides)
    }

    /// Applies this override to `plan` in place.
    ///
    /// `plan: &mut Plan` is a *mutable borrow*: this function may change the
    /// plan, but doesn't own it. While it holds that borrow, nothing else can
    /// read or write the plan, which is how Rust rules out data races at
    /// compile time. Errors are plain problem text; [`apply_overrides`] adds
    /// which override it was.
    fn apply_to(&self, plan: &mut Plan) -> Result<(), String> {
        match self {
            // In a `match` on `&Override`, the bindings (`years`,
            // `person_id`) are references into the override: `&i32` and
            // `&Option<String>`. `*years` reads the number.
            Override::ShiftRetirement { years, person_id } => {
                require_phases(plan)?;
                let final_year = plan.last_retirement_year() + plan.drawdown_years;
                match person_id {
                    Some(id) => {
                        // `iter_mut()` yields `&mut PersonInput`, so the
                        // person found can be changed in place.
                        // `ok_or_else` turns `None` into an `Err`.
                        let person = plan
                            .people
                            .iter_mut()
                            .find(|person| person.id == *id)
                            .ok_or_else(|| format!("no person with id `{id}`"))?;
                        person.saving_years = add_years(person.saving_years, *years)?;
                    }
                    None if plan.people.is_empty() => {
                        plan.saving_years = add_years(plan.saving_years, *years)?;
                    }
                    // A *match guard* (`if ...` above) lets an arm apply only
                    // when a condition holds; this arm catches the rest.
                    None => {
                        for person in &mut plan.people {
                            person.saving_years = add_years(person.saving_years, *years)?;
                        }
                    }
                }
                // Keep the final year fixed: drawdown absorbs the shift.
                plan.drawdown_years = final_year - plan.last_retirement_year();
            }

            Override::SetDrawdownYears { years } => {
                require_phases(plan)?;
                plan.drawdown_years = *years;
            }

            Override::SetAnnualWithdrawal {
                annual_withdrawal_cents,
            } => {
                require_phases(plan)?;
                if !plan.expenses.is_empty() {
                    return Err(
                        "this plan itemizes expenses; use scale_expenses or set_expense instead"
                            .to_string(),
                    );
                }
                plan.annual_withdrawal_cents = *annual_withdrawal_cents;
            }

            Override::ScaleExpenses { percent } => {
                require_phases(plan)?;
                if !(0..=MAX_SCALE_PERCENT).contains(percent) {
                    return Err(format!("percent must be between 0 and {MAX_SCALE_PERCENT}"));
                }
                for expense in &mut plan.expenses {
                    expense.annual_amount_cents =
                        scale_cents(expense.annual_amount_cents, *percent)?;
                }
                plan.annual_withdrawal_cents = scale_cents(plan.annual_withdrawal_cents, *percent)?;
                plan.gap_annual_withdrawal_cents =
                    scale_cents(plan.gap_annual_withdrawal_cents, *percent)?;
            }

            Override::AddExpense { expense } => {
                require_phases(plan)?;
                if !expense.id.is_empty() && plan.expenses.iter().any(|e| e.id == expense.id) {
                    return Err(format!(
                        "an expense with id `{}` already exists",
                        expense.id
                    ));
                }
                // `expense` is borrowed from the override, so it's cloned to
                // give the plan its own copy. The override stays usable (it's
                // echoed back in the result).
                plan.expenses.push(expense.clone());
            }

            Override::RemoveExpense { expense_id } => {
                let index = plan
                    .expenses
                    .iter()
                    .position(|expense| expense.id == *expense_id)
                    .ok_or_else(|| format!("no expense with id `{expense_id}`"))?;
                plan.expenses.remove(index);
            }

            Override::SetExpense {
                expense_id,
                annual_amount_cents,
                inflation_rate_basis_points,
                from_year,
                to_year,
            } => {
                if annual_amount_cents.is_none()
                    && inflation_rate_basis_points.is_none()
                    && from_year.is_none()
                    && to_year.is_none()
                {
                    return Err("give at least one field to change".to_string());
                }
                let expense = find_expense(plan, expense_id)?;
                // `if let Some(x) = option` runs only when there's a value.
                if let Some(amount) = annual_amount_cents {
                    expense.annual_amount_cents = *amount;
                }
                if let Some(rate) = inflation_rate_basis_points {
                    expense.inflation_rate_basis_points = Some(*rate);
                }
                if let Some(year) = from_year {
                    expense.from_year = *year;
                }
                if let Some(year) = to_year {
                    expense.to_year = *year;
                    // `to_year` replaces the older `years` field.
                    expense.years = 0;
                }
            }

            Override::AdjustReturns {
                basis_points,
                drawdown_only,
            } => {
                if !(-MAX_RETURN_ADJUSTMENT..=MAX_RETURN_ADJUSTMENT).contains(basis_points) {
                    return Err(format!(
                        "basisPoints must be between -{MAX_RETURN_ADJUSTMENT} and {MAX_RETURN_ADJUSTMENT}"
                    ));
                }
                for item in &mut plan.items {
                    if *drawdown_only {
                        // No separate drawdown rate means drawdown uses the
                        // saving rate, so start from that.
                        let current = item
                            .drawdown_annual_return_rate_basis_points
                            .unwrap_or(item.annual_return_rate_basis_points);
                        item.drawdown_annual_return_rate_basis_points =
                            Some(current + basis_points);
                    } else {
                        item.annual_return_rate_basis_points += basis_points;
                        // `Option::map` changes the value inside a `Some` and
                        // leaves a `None` alone: no separate drawdown rate
                        // keeps following the saving rate.
                        item.drawdown_annual_return_rate_basis_points = item
                            .drawdown_annual_return_rate_basis_points
                            .map(|rate| rate + basis_points);
                    }
                }
            }

            Override::SetItem {
                item_id,
                annual_contribution_cents,
                annual_return_rate_basis_points,
                drawdown_annual_return_rate_basis_points,
                withdrawal_priority,
            } => {
                if annual_contribution_cents.is_none()
                    && annual_return_rate_basis_points.is_none()
                    && drawdown_annual_return_rate_basis_points.is_none()
                    && withdrawal_priority.is_none()
                {
                    return Err("give at least one field to change".to_string());
                }
                if let Some(priority) = withdrawal_priority
                    && !(1..=3).contains(priority)
                {
                    return Err("withdrawalPriority must be 1, 2 or 3".to_string());
                }
                let item = plan
                    .items
                    .iter_mut()
                    .find(|item| !item.id.is_empty() && item.id == *item_id)
                    .ok_or_else(|| {
                        format!(
                            "no item with id `{item_id}` (items without an id can't be changed)"
                        )
                    })?;
                if let Some(cents) = annual_contribution_cents {
                    item.annual_contribution_cents = *cents;
                }
                if let Some(rate) = annual_return_rate_basis_points {
                    item.annual_return_rate_basis_points = *rate;
                }
                if let Some(rate) = drawdown_annual_return_rate_basis_points {
                    item.drawdown_annual_return_rate_basis_points = Some(*rate);
                }
                if let Some(priority) = withdrawal_priority {
                    item.withdrawal_priority = *priority;
                }
            }

            Override::SetInflation { basis_points } => {
                require_phases(plan)?;
                plan.annual_withdrawal_inflation_rate_basis_points = *basis_points;
            }
        }
        Ok(())
    }
}

/// Applies `overrides` in order to a copy of `plan` and returns the copy.
///
/// `plan: &Plan` is a shared borrow, so this function *can't* change the
/// caller's plan: the compiler enforces it. `plan.clone()` makes an
/// independent deep copy (every `Vec` and `String` inside is copied too) that
/// the overrides then change through `&mut`.
///
/// The base plan is checked first, so its own problems are reported as they
/// are. After that, the plan is re-checked after each override, so a result
/// that breaks a rule (saving years below 0, say) names the override that
/// caused it.
pub fn apply_overrides(plan: &Plan, overrides: &[Override]) -> Result<Plan, Error> {
    plan.check()?;
    let mut changed = plan.clone();
    for (index, change) in overrides.iter().enumerate() {
        // A closure that builds this override's error from a problem text.
        let at_fault = |problem: String| Error::InvalidOverride {
            index,
            op: change.op_name(),
            problem,
        };
        change.apply_to(&mut changed).map_err(at_fault)?;
        if let Err(Error::InvalidPlan { problems }) = changed.check() {
            return Err(at_fault(format!(
                "the changed plan breaks a rule: {}",
                problems.join("; ")
            )));
        }
    }
    Ok(changed)
}

/// Fails for simple `years` plans, which have no retirement or drawdown.
fn require_phases(plan: &Plan) -> Result<(), String> {
    if plan.uses_phase_projection() {
        Ok(())
    } else {
        Err(
            "only works for plans with saving and drawdown phases (this plan uses `years`)"
                .to_string(),
        )
    }
}

fn find_expense<'p>(plan: &'p mut Plan, id: &str) -> Result<&'p mut ExpenseInput, String> {
    // `'p` is a *lifetime*: it says the returned reference points into
    // `plan`, so it can't outlive it. Rust usually infers lifetimes; here it
    // can't tell whether the result borrows from `plan` or from `id`, so it's
    // written out.
    plan.expenses
        .iter_mut()
        .find(|expense| !expense.id.is_empty() && expense.id == id)
        .ok_or_else(|| format!("no expense with id `{id}`"))
}

/// Adds years without overflowing.
///
/// Integer overflow in Rust panics in debug builds and silently wraps in
/// release builds; `checked_add` returns `None` instead, so a huge `years`
/// becomes a clear error either way.
fn add_years(current: i32, years: i32) -> Result<i32, String> {
    current
        .checked_add(years)
        .ok_or_else(|| "years is out of range".to_string())
}

/// `cents * percent / 100`, rounded half away from zero, without overflow.
fn scale_cents(cents: i64, percent: i32) -> Result<i64, String> {
    let out_of_range = || "the scaled amount is out of range".to_string();
    let product = cents
        .checked_mul(i64::from(percent))
        .ok_or_else(out_of_range)?;
    // Integer division truncates toward zero, so add or subtract half the
    // divisor first to round. (Amounts are already checked to be >= 0.)
    let half = if product < 0 { -50 } else { 50 };
    Ok(product.checked_add(half).ok_or_else(out_of_range)? / 100)
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = include_str!("../tests/fixtures/plan-basic.json");
    const HOUSEHOLD: &str = include_str!("../tests/fixtures/plan-household.json");

    fn basic() -> Plan {
        Plan::from_json_checked(BASIC).unwrap()
    }

    fn household() -> Plan {
        Plan::from_json_checked(HOUSEHOLD).unwrap()
    }

    /// Parses overrides from JSON text, as a tool call would send them.
    fn ops(json: &str) -> Vec<Override> {
        Override::list_from_value(serde_json::from_str(json).unwrap()).unwrap()
    }

    fn apply(plan: &Plan, json: &str) -> Plan {
        apply_overrides(plan, &ops(json)).expect("overrides should apply")
    }

    fn apply_err(plan: &Plan, json: &str) -> String {
        apply_overrides(plan, &ops(json))
            .expect_err("overrides should fail")
            .to_string()
    }

    // ---- JSON shape -------------------------------------------------------

    #[test]
    fn op_tag_selects_the_variant() {
        let parsed = ops(r#"[
                {"op": "shift_retirement", "years": -2},
                {"op": "shift_retirement", "years": 1, "personId": "person-a"},
                {"op": "adjust_returns", "basisPoints": -100, "drawdownOnly": true}
            ]"#);
        assert_eq!(
            parsed,
            [
                Override::ShiftRetirement {
                    years: -2,
                    person_id: None
                },
                Override::ShiftRetirement {
                    years: 1,
                    person_id: Some("person-a".to_string())
                },
                Override::AdjustReturns {
                    basis_points: -100,
                    drawdown_only: true
                },
            ]
        );
    }

    #[test]
    fn overrides_serialize_back_to_the_same_shape() {
        let json = serde_json::to_value(Override::SetInflation { basis_points: 300 }).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op": "set_inflation", "basisPoints": 300})
        );
        // Unset optional fields are left out.
        let json = serde_json::to_value(Override::AdjustReturns {
            basis_points: 50,
            drawdown_only: false,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op": "adjust_returns", "basisPoints": 50})
        );
    }

    #[test]
    fn bad_override_json_names_the_place_but_not_the_value() {
        let shape = |json: &str| {
            Override::list_from_value(serde_json::from_str(json).unwrap())
                .unwrap_err()
                .to_string()
        };

        let message = shape(r#"[{"op": "retire_early", "years": 2}]"#);
        assert!(
            message.starts_with(
                "override problem at `overrides[0].op`: unknown variant `retire_early`"
            ),
            "{message}"
        );
        assert!(
            message.contains("shift_retirement"),
            "lists the valid ops: {message}"
        );

        // With an `op` tag, serde reads the whole object before it knows the
        // variant, so field problems point at the override, not the field.
        let message = shape(r#"[{"op": "set_drawdown_years", "yeers": 25}]"#);
        assert_eq!(
            message,
            "override problem at `overrides[0]`: unknown field `yeers`, expected `years`"
        );

        let message = shape(r#"[{"op": "set_inflation", "basisPoints": 2.75}]"#);
        assert!(message.contains("expected a whole number"), "{message}");
        assert!(!message.contains("2.75"), "{message}");

        let message = shape(r#"{"op": "set_inflation", "basisPoints": 300}"#);
        assert!(message.contains("expected a list"), "{message}");
    }

    #[test]
    fn too_many_overrides_are_rejected() {
        let many = serde_json::Value::Array(
            (0..=MAX_OVERRIDES)
                .map(|_| serde_json::json!({"op": "set_inflation", "basisPoints": 300}))
                .collect(),
        );
        let err = Override::list_from_value(many).unwrap_err();
        assert_eq!(
            err.to_string(),
            "override problem at `overrides`: at most 20 overrides are allowed"
        );
    }

    // ---- shift_retirement -------------------------------------------------

    #[test]
    fn shift_retirement_keeps_the_final_year() {
        let original = basic();
        let changed = apply(&original, r#"[{"op": "shift_retirement", "years": -2}]"#);

        assert_eq!(changed.saving_years, 8);
        assert_eq!(changed.drawdown_years, 32);
        // Nothing else changed: compare with only the expected fields edited.
        let mut expected = original.clone();
        expected.saving_years = 8;
        expected.drawdown_years = 32;
        assert_eq!(changed, expected);
    }

    #[test]
    fn shift_retirement_moves_everyone_or_one_person() {
        let plan = household(); // A retires after 8 years, B after 11; 30 drawdown.

        let everyone = apply(&plan, r#"[{"op": "shift_retirement", "years": 1}]"#);
        assert_eq!(everyone.people[0].saving_years, 9);
        assert_eq!(everyone.people[1].saving_years, 12);
        assert_eq!(everyone.drawdown_years, 29);

        // Moving A doesn't move the last retirement (B), so drawdown stays.
        let one = apply(
            &plan,
            r#"[{"op": "shift_retirement", "years": -3, "personId": "person-a"}]"#,
        );
        assert_eq!(one.people[0].saving_years, 5);
        assert_eq!(one.people[1].saving_years, 11);
        assert_eq!(one.drawdown_years, 30);

        // Moving B past nobody: B is still last, so drawdown absorbs it.
        let b = apply(
            &plan,
            r#"[{"op": "shift_retirement", "years": -4, "personId": "person-b"}]"#,
        );
        assert_eq!(b.people[1].saving_years, 7);
        // Now A (8) retires last: final year 41 - 8 = 33 drawdown years.
        assert_eq!(b.drawdown_years, 33);
    }

    #[test]
    fn shift_retirement_errors_name_the_override() {
        assert_eq!(
            apply_err(
                &household(),
                r#"[{"op": "set_inflation", "basisPoints": 300},
                    {"op": "shift_retirement", "years": 1, "personId": "person-c"}]"#
            ),
            "overrides[1] (shift_retirement): no person with id `person-c`"
        );
        assert_eq!(
            apply_err(&basic(), r#"[{"op": "shift_retirement", "years": -11}]"#),
            "overrides[0] (shift_retirement): the changed plan breaks a rule: savingYears must be between 0 and 75"
        );
        assert_eq!(
            apply_err(
                &basic(),
                r#"[{"op": "shift_retirement", "years": 2147483647}]"#
            ),
            "overrides[0] (shift_retirement): years is out of range"
        );
    }

    #[test]
    fn phase_ops_reject_simple_year_plans() {
        let mut simple = basic();
        simple.saving_years = 0;
        simple.drawdown_years = 0;
        simple.annual_withdrawal_cents = 0;
        simple.annual_withdrawal_inflation_rate_basis_points = 0;
        simple.years = 30;

        assert_eq!(
            apply_err(&simple, r#"[{"op": "shift_retirement", "years": 1}]"#),
            "overrides[0] (shift_retirement): only works for plans with saving and drawdown phases (this plan uses `years`)"
        );
        // Return changes work for any plan.
        let changed = apply(&simple, r#"[{"op": "adjust_returns", "basisPoints": 100}]"#);
        assert_eq!(changed.items[0].annual_return_rate_basis_points, 800);
    }

    // ---- drawdown, withdrawal, inflation -----------------------------------

    #[test]
    fn set_drawdown_years_withdrawal_and_inflation() {
        let changed = apply(
            &basic(),
            r#"[{"op": "set_drawdown_years", "years": 25},
                {"op": "set_annual_withdrawal", "annualWithdrawalCents": 5000000},
                {"op": "set_inflation", "basisPoints": 300}]"#,
        );
        assert_eq!(changed.drawdown_years, 25);
        assert_eq!(changed.annual_withdrawal_cents, 5_000_000);
        assert_eq!(changed.annual_withdrawal_inflation_rate_basis_points, 300);

        assert_eq!(
            apply_err(&basic(), r#"[{"op": "set_drawdown_years", "years": 76}]"#),
            "overrides[0] (set_drawdown_years): the changed plan breaks a rule: drawdownYears must be between 0 and 75"
        );
    }

    #[test]
    fn set_annual_withdrawal_refuses_itemized_plans() {
        assert_eq!(
            apply_err(
                &household(),
                r#"[{"op": "set_annual_withdrawal", "annualWithdrawalCents": 1}]"#
            ),
            "overrides[0] (set_annual_withdrawal): this plan itemizes expenses; use scale_expenses or set_expense instead"
        );
    }

    // ---- spending ----------------------------------------------------------

    #[test]
    fn scale_expenses_scales_itemized_and_gap_spending() {
        let plan = household();
        let changed = apply(&plan, r#"[{"op": "scale_expenses", "percent": 90}]"#);

        let amounts: Vec<i64> = changed
            .expenses
            .iter()
            .map(|expense| expense.annual_amount_cents)
            .collect();
        // 55,000 / 12,000 / 10,000 dollars, each 10% lower (in cents).
        assert_eq!(amounts, [4_950_000, 1_080_000, 900_000]);
        assert_eq!(changed.gap_annual_withdrawal_cents, 2_250_000);
        // The mortgage isn't spending that can be cut.
        assert_eq!(changed.mortgage, plan.mortgage);
    }

    #[test]
    fn scale_expenses_scales_the_withdrawal_without_itemized_expenses() {
        let changed = apply(&basic(), r#"[{"op": "scale_expenses", "percent": 110}]"#);
        assert_eq!(changed.annual_withdrawal_cents, 6_600_000);
    }

    #[test]
    fn scaling_rounds_half_away_from_zero_and_checks_range() {
        assert_eq!(scale_cents(1, 50), Ok(1)); // 0.5 cents rounds up
        assert_eq!(scale_cents(1, 49), Ok(0));
        assert_eq!(scale_cents(999, 0), Ok(0));
        assert!(scale_cents(i64::MAX, 2).is_err());

        assert_eq!(
            apply_err(&basic(), r#"[{"op": "scale_expenses", "percent": -10}]"#),
            "overrides[0] (scale_expenses): percent must be between 0 and 500"
        );
    }

    #[test]
    fn add_set_and_remove_expenses() {
        let plan = household();
        let changed = apply(
            &plan,
            r#"[
                {"op": "add_expense", "expense": {"id": "expense-car", "name": "New car", "annualAmountCents": 800000, "fromYear": 5, "toYear": 5}},
                {"op": "set_expense", "expenseId": "expense-travel", "toYear": 15, "annualAmountCents": 1500000},
                {"op": "remove_expense", "expenseId": "expense-health"}
            ]"#,
        );

        let ids: Vec<&str> = changed.expenses.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["expense-living", "expense-travel", "expense-car"]);
        let travel = &changed.expenses[1];
        assert_eq!(travel.to_year, 15);
        assert_eq!(travel.from_year, 1, "unchanged fields stay");
        assert_eq!(travel.annual_amount_cents, 1_500_000);
        assert_eq!(changed.expenses[2].from_year, 5);
    }

    #[test]
    fn expense_errors() {
        let plan = household();
        assert_eq!(
            apply_err(
                &plan,
                r#"[{"op": "remove_expense", "expenseId": "expense-boat"}]"#
            ),
            "overrides[0] (remove_expense): no expense with id `expense-boat`"
        );
        assert_eq!(
            apply_err(
                &plan,
                r#"[{"op": "set_expense", "expenseId": "expense-travel"}]"#
            ),
            "overrides[0] (set_expense): give at least one field to change"
        );
        assert_eq!(
            apply_err(
                &plan,
                r#"[{"op": "add_expense", "expense": {"id": "expense-travel", "name": "Again", "annualAmountCents": 1}}]"#
            ),
            "overrides[0] (add_expense): an expense with id `expense-travel` already exists"
        );
        // The rule check catches what the op itself doesn't: the message
        // carries the field rule, not the amount.
        let message = apply_err(
            &plan,
            r#"[{"op": "set_expense", "expenseId": "expense-living", "annualAmountCents": -424242}]"#,
        );
        assert_eq!(
            message,
            "overrides[0] (set_expense): the changed plan breaks a rule: expenses[0].annualAmountCents must be greater than or equal to 0"
        );
        assert!(!message.contains("424242"));
    }

    #[test]
    fn adding_an_expense_to_a_withdrawal_plan_breaks_a_rule() {
        assert_eq!(
            apply_err(
                &basic(),
                r#"[{"op": "add_expense", "expense": {"name": "Boat", "annualAmountCents": 1}}]"#
            ),
            "overrides[0] (add_expense): the changed plan breaks a rule: annualWithdrawalCents must be 0 when expenses are provided"
        );
    }

    // ---- returns and items -------------------------------------------------

    #[test]
    fn adjust_returns_moves_every_rate_together() {
        let changed = apply(
            &household(),
            r#"[{"op": "adjust_returns", "basisPoints": -100}]"#,
        );
        let rates: Vec<(i32, Option<i32>)> = changed
            .items
            .iter()
            .map(|item| {
                (
                    item.annual_return_rate_basis_points,
                    item.drawdown_annual_return_rate_basis_points,
                )
            })
            .collect();
        // A separate drawdown rate moves too; `None` keeps following.
        assert_eq!(rates, [(600, Some(400)), (550, None), (200, None)]);
    }

    #[test]
    fn adjust_returns_drawdown_only() {
        let changed = apply(
            &household(),
            r#"[{"op": "adjust_returns", "basisPoints": -150, "drawdownOnly": true}]"#,
        );
        let rates: Vec<(i32, Option<i32>)> = changed
            .items
            .iter()
            .map(|item| {
                (
                    item.annual_return_rate_basis_points,
                    item.drawdown_annual_return_rate_basis_points,
                )
            })
            .collect();
        // Saving rates are untouched; drawdown starts from its own rate or,
        // without one, from the saving rate.
        assert_eq!(
            rates,
            [(700, Some(350)), (650, Some(500)), (300, Some(150))]
        );

        assert_eq!(
            apply_err(
                &household(),
                r#"[{"op": "adjust_returns", "basisPoints": 5000}]"#
            ),
            "overrides[0] (adjust_returns): basisPoints must be between -2000 and 2000"
        );
    }

    #[test]
    fn set_item_changes_one_account() {
        let plan = basic();
        let changed = apply(
            &plan,
            r#"[{"op": "set_item", "itemId": "item-roth", "annualContributionCents": 0, "withdrawalPriority": 1}]"#,
        );
        let mut expected = plan.clone();
        expected.items[1].annual_contribution_cents = 0;
        expected.items[1].withdrawal_priority = 1;
        assert_eq!(changed, expected);

        assert_eq!(
            apply_err(
                &plan,
                r#"[{"op": "set_item", "itemId": "item-roth", "withdrawalPriority": 7}]"#
            ),
            "overrides[0] (set_item): withdrawalPriority must be 1, 2 or 3"
        );
        assert_eq!(
            apply_err(
                &plan,
                r#"[{"op": "set_item", "itemId": "", "annualContributionCents": 1}]"#
            ),
            "overrides[0] (set_item): no item with id `` (items without an id can't be changed)"
        );
    }

    // ---- applying in general ------------------------------------------------

    #[test]
    fn overrides_apply_in_order() {
        let plan = basic();
        // Retire later, then set drawdown: the second op wins for drawdown.
        let later_then_set = apply(
            &plan,
            r#"[{"op": "shift_retirement", "years": 2}, {"op": "set_drawdown_years", "years": 30}]"#,
        );
        assert_eq!(
            (later_then_set.saving_years, later_then_set.drawdown_years),
            (12, 30)
        );

        // The other way round, the shift adjusts drawdown after it was set.
        let set_then_later = apply(
            &plan,
            r#"[{"op": "set_drawdown_years", "years": 30}, {"op": "shift_retirement", "years": 2}]"#,
        );
        assert_eq!(
            (set_then_later.saving_years, set_then_later.drawdown_years),
            (12, 28)
        );

        // Two scales compound.
        let twice = apply(
            &plan,
            r#"[{"op": "scale_expenses", "percent": 50}, {"op": "scale_expenses", "percent": 50}]"#,
        );
        assert_eq!(twice.annual_withdrawal_cents, 1_500_000);
    }

    #[test]
    fn the_original_plan_is_never_changed() {
        let original = household();
        let before = original.clone();
        let _ = apply(
            &original,
            r#"[{"op": "shift_retirement", "years": -2},
                {"op": "scale_expenses", "percent": 80},
                {"op": "adjust_returns", "basisPoints": -100},
                {"op": "remove_expense", "expenseId": "expense-travel"}]"#,
        );
        // A failing override leaves it alone too.
        let _ = apply_overrides(
            &original,
            &ops(
                r#"[{"op": "set_drawdown_years", "years": 10}, {"op": "remove_expense", "expenseId": "nope"}]"#,
            ),
        );
        assert_eq!(original, before);
    }

    #[test]
    fn no_overrides_returns_an_equal_copy() {
        assert_eq!(apply_overrides(&basic(), &[]).unwrap(), basic());
    }

    #[test]
    fn an_invalid_base_plan_is_reported_as_itself() {
        let mut plan = basic();
        plan.drawdown_years = 99;
        let err = apply_overrides(
            &plan,
            &ops(r#"[{"op": "set_inflation", "basisPoints": 1}]"#),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid plan: drawdownYears must be between 0 and 75"
        );
    }
}
