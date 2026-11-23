//! **Withdrawal order options**: which account should the money come from
//! first?
//!
//! financials-api orders withdrawals by each account's `withdrawalPriority`:
//! 1 = first, 2 = normal (also 0 / omitted), 3 = last. Withdrawals come from
//! the lowest priority *group* first, and accounts in the same group share
//! each withdrawal in proportion to their balances.
//!
//! Only the order of the groups matters, not the numbers: `[1, 2]` and
//! `[2, 3]` both mean "account A, then account B". So each assignment is
//! reduced to a canonical **order** (its dense ranks: `[1, 2]`), and
//! candidates with the same order are duplicates.
//!
//! [`withdrawal_order_options`] builds the meaningful candidates (the current
//! order, all accounts together, each account first, each account last, and,
//! for up to 3 accounts, every strict order), keeps at most [`MAX_OPTIONS`]
//! distinct ones, projects them concurrently, and ranks them by how long the
//! money lasts, then by ending balance.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;

use crate::batch::project_all;
use crate::compare::Change;
use crate::error::Error;
use crate::plan::Plan;
use crate::projector::Projector;
use crate::summary::{Headline, Summary};

/// Most orders compared in one call, including the current one. Three
/// accounts have exactly 13 distinct orders (every way to split them into
/// ordered groups), so 13 compares all of them; it also covers "together",
/// each-first and each-last for up to 6 accounts. It matches the 13
/// projections `compare_scenarios` allows (a baseline plus 12).
pub const MAX_OPTIONS: usize = 13;

/// financials-api has three priority levels, so a strict order (no two
/// accounts sharing a group) exists only for up to three accounts.
const PRIORITY_LEVELS: usize = 3;

/// A canonical withdrawal order: each account's group, numbered 1, 2, 3 from
/// first to last with no gaps. `[2, 1, 2]` means "the second account first,
/// then the other two together".
///
/// A *type alias* gives an existing type a descriptive name, like Go's
/// `type Order = []int` (with `=`): it's still just a `Vec<u8>`.
type Order = Vec<u8>;

/// Reduces priorities to a canonical [`Order`] by *dense ranking*: the
/// smallest distinct priority becomes 1, the next 2, and so on. `0` counts as
/// 2 (normal), as in financials-api.
fn canonical(priorities: &[i32]) -> Order {
    let effective: Vec<i32> = priorities
        .iter()
        .map(|&priority| if priority == 0 { 2 } else { priority })
        .collect();
    let mut distinct = effective.clone();
    // `sort_unstable` + `dedup` leaves each distinct value once, in order.
    distinct.sort_unstable();
    distinct.dedup();
    effective
        .iter()
        .map(|priority| {
            // `binary_search` returns `Ok(index)` when found; the value is
            // always present here, since `distinct` came from `effective`.
            let index = distinct
                .binary_search(priority)
                .expect("every priority is in the distinct list");
            // `u8::try_from` can't fail: there are at most 3 distinct values.
            u8::try_from(index + 1).expect("at most three groups")
        })
        .collect()
}

/// All orderings of `0..n`, for small `n`.
///
/// A recursive function: for each first element, put it in front of every
/// ordering of the rest. `Vec<Vec<usize>>` is a list of lists.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    fn extend(prefix: &mut Vec<usize>, remaining: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if remaining.is_empty() {
            out.push(prefix.clone());
            return;
        }
        for position in 0..remaining.len() {
            let next = remaining.remove(position);
            prefix.push(next);
            extend(prefix, remaining, out);
            // Undo the choice before trying the next one (backtracking).
            prefix.pop();
            remaining.insert(position, next);
        }
    }
    let mut out = Vec::new();
    extend(&mut Vec::new(), &mut (0..n).collect(), &mut out);
    out
}

/// The candidate orders, current first, without duplicates, capped at
/// [`MAX_OPTIONS`]. Returns them and whether any were left out.
fn candidates(plan: &Plan) -> (Vec<Order>, bool) {
    let count = plan.items.len();
    let current: Vec<i32> = plan
        .items
        .iter()
        .map(|item| item.withdrawal_priority)
        .collect();

    // Every candidate, in the order they're preferred when the cap applies.
    let mut all: Vec<Order> = vec![canonical(&current), vec![1; count]];
    for chosen in 0..count {
        // `(0..count).map(...).collect()` builds a list with one entry per
        // account: 1 for the chosen account, 2 for the rest.
        all.push(
            (0..count)
                .map(|i| if i == chosen { 1 } else { 2 })
                .collect(),
        );
    }
    for chosen in 0..count {
        all.push(
            (0..count)
                .map(|i| if i == chosen { 2 } else { 1 })
                .collect(),
        );
    }
    if count <= PRIORITY_LEVELS {
        for permutation in permutations(count) {
            // `permutation[k]` is the account in position k; its group is
            // k + 1.
            let mut order = vec![0; count];
            for (position, &account) in permutation.iter().enumerate() {
                order[account] = u8::try_from(position + 1).expect("at most three accounts");
            }
            all.push(order);
        }
    }

    // `HashSet::insert` returns `false` for a value already present, so
    // `retain` keeps only each order's first appearance, preserving order.
    let mut seen: HashSet<Order> = HashSet::new();
    all.retain(|order| seen.insert(order.clone()));

    let truncated = all.len() > MAX_OPTIONS;
    all.truncate(MAX_OPTIONS);
    (all, truncated)
}

/// Describes an order in words, using account names: "Brokerage first, then
/// 401(k) and Roth IRA together".
fn describe(order: &[u8], plan: &Plan) -> String {
    if let [only] = plan.items.as_slice() {
        return format!("{} (the only account)", only.name);
    }
    let groups = order.iter().copied().max().unwrap_or(1);
    if groups == 1 {
        return "all accounts together, in proportion to their balances".to_string();
    }
    let names_in = |group: u8| -> String {
        let names: Vec<&str> = order
            .iter()
            .zip(&plan.items)
            .filter(|(g, _)| **g == group)
            .map(|(_, item)| item.name.as_str())
            .collect();
        match names.as_slice() {
            [one] => one.to_string(),
            [rest @ .., last] => format!("{} and {last} together", rest.join(", ")),
            [] => String::new(),
        }
    };
    let parts: Vec<String> = (1..=groups).map(names_in).collect();
    format!("{} first, then {}", parts[0], parts[1..].join(", then "))
}

/// The plan with each account's priority set from `order` (one group means
/// everyone is "normal").
fn plan_with(plan: &Plan, order: &[u8]) -> Plan {
    let single_group = order.iter().all(|&group| group == 1);
    let mut changed = plan.clone();
    for (item, &group) in changed.items.iter_mut().zip(order) {
        item.withdrawal_priority = if single_group { 2 } else { i32::from(group) };
    }
    changed
}

/// The options, best first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WithdrawalOrderOptions {
    pub options: Vec<OrderOption>,
    /// True when more distinct orders existed than were compared.
    pub truncated: bool,
    /// An explanation when there's nothing to choose between.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One withdrawal order and how it did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderOption {
    /// 1 = best. Failed options come last, unranked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    pub description: String,
    pub is_current: bool,
    /// Each account's `withdrawalPriority` for this order, ready to use with
    /// the `set_item` override.
    pub priorities: Vec<AccountPriority>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPriority {
    pub item: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub item_id: String,
    pub withdrawal_priority: i32,
}

#[derive(Debug, Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum Outcome {
    Projected {
        headline: Headline,
        change_from_current: Change,
    },
    Failed {
        error: String,
    },
}

/// The ranking key: lasts through the final year first, then the latest
/// depletion year, fewest years short, highest ending balance, least
/// unfunded spending.
///
/// Tuples compare element by element, like Python's. `Reverse(x)` flips the
/// order of one element, so "bigger is better" values sort first.
fn rank_key(summary: &Summary) -> (Reverse<bool>, Reverse<i32>, usize, Reverse<i64>, i64) {
    (
        Reverse(summary.lasts_through_final_year),
        Reverse(summary.depletion_year.unwrap_or(i32::MAX)),
        summary.years_short,
        Reverse(summary.ending_balance),
        summary.total_unfunded,
    )
}

/// Compares the meaningful withdrawal orders for `plan`, best first.
///
/// The current order is projected exactly as given and must succeed, since
/// every other option is compared with it. Other options that fail are
/// listed last with their error.
pub async fn withdrawal_order_options<P: Projector + 'static>(
    projector: Arc<P>,
    plan: &Plan,
) -> Result<WithdrawalOrderOptions, Error> {
    plan.check()?;

    let (orders, truncated) = if plan.items.len() < 2 {
        (vec![canonical(&[0])], false)
    } else {
        candidates(plan)
    };

    // The current order runs on the plan exactly as given; the others on a
    // copy with their priorities.
    let prepared: Vec<Result<Plan, Error>> = orders
        .iter()
        .enumerate()
        .map(|(index, order)| {
            Ok(if index == 0 {
                plan.clone()
            } else {
                plan_with(plan, order)
            })
        })
        .collect();
    let priorities: Vec<Vec<i32>> = prepared
        .iter()
        .map(|run| match run {
            Ok(run_plan) => run_plan
                .items
                .iter()
                .map(|item| item.withdrawal_priority)
                .collect(),
            Err(_) => Vec::new(),
        })
        .collect();

    let mut results = project_all(projector, prepared).await;
    // `remove(0)` takes the first element out of the `Vec`, shifting the
    // rest down; `?` fails the call if the current order failed.
    let current = results.remove(0)?;
    let results = std::iter::once(Ok(current.clone())).chain(results);

    // Pair each order with its result, then sort. Successful options are
    // ranked; failed ones go last in generation order. `sort_by_key` is a
    // *stable* sort: ties keep their original order, so the current order
    // wins a tie with an equivalent one.
    let mut scored: Vec<(usize, Result<Summary, Error>)> = results.enumerate().collect();
    scored.sort_by_key(|(index, result)| match result {
        Ok(summary) => (0, Some(rank_key(summary)), *index),
        Err(_) => (1, None, *index),
    });

    let mut rank = 0;
    let options = scored
        .into_iter()
        .map(|(index, result)| {
            let outcome = match result {
                Ok(summary) => {
                    rank += 1;
                    Outcome::Projected {
                        headline: Headline::from(&summary),
                        change_from_current: Change::between(&current, &summary),
                    }
                }
                Err(err) => Outcome::Failed {
                    error: err.to_string(),
                },
            };
            OrderOption {
                rank: matches!(outcome, Outcome::Projected { .. }).then_some(rank),
                description: describe(&orders[index], plan),
                is_current: index == 0,
                priorities: plan
                    .items
                    .iter()
                    .zip(&priorities[index])
                    .map(|(item, &priority)| AccountPriority {
                        item: item.name.clone(),
                        item_id: item.id.clone(),
                        withdrawal_priority: if priority == 0 { 2 } else { priority },
                    })
                    .collect(),
                outcome,
            }
        })
        .collect();

    Ok(WithdrawalOrderOptions {
        options,
        truncated,
        note: (plan.items.len() < 2).then(|| {
            "The plan has only one account, so the withdrawal order can't change anything."
                .to_string()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::ItemInput;
    use crate::projection::Projection;
    use crate::projector::FakeProjector;

    const BASIC: &str = include_str!("../tests/fixtures/plan-basic.json");
    const PROJECTION_BASIC: &str = include_str!("../tests/fixtures/projection-basic.json");

    fn basic() -> Plan {
        Plan::from_json_checked(BASIC).unwrap()
    }

    /// The basic plan with `count` accounts (copies of the first one).
    fn with_accounts(count: usize) -> Plan {
        let mut plan = basic();
        let template: ItemInput = plan.items[0].clone();
        plan.items = (0..count)
            .map(|n| ItemInput {
                id: format!("item-{n}"),
                name: format!("Account {n}"),
                withdrawal_priority: 0,
                sort_order: n as i32,
                ..template.clone()
            })
            .collect();
        plan
    }

    /// A projection that runs short from `depletion` (or lasts, for `None`)
    /// and ends with `ending` dollars.
    fn outcome(depletion: Option<i32>, ending: i64) -> Projection {
        let mut projection: Projection = serde_json::from_str(PROJECTION_BASIC).unwrap();
        for year in &mut projection.totals {
            let short = depletion.is_some_and(|d| year.year >= d);
            year.unfunded_withdrawal_cents = if short { 100 } else { 0 };
        }
        projection.totals.last_mut().unwrap().balance_cents = ending * 100;
        projection
    }

    #[test]
    fn canonical_orders_ignore_the_numbers() {
        assert_eq!(canonical(&[1, 2]), [1, 2]);
        assert_eq!(canonical(&[2, 3]), [1, 2]);
        assert_eq!(canonical(&[0, 3, 1]), [2, 3, 1]);
        assert_eq!(canonical(&[0, 2, 0]), [1, 1, 1]);
        assert_eq!(canonical(&[3, 3]), [1, 1]);
    }

    #[test]
    fn permutations_cover_every_order_once() {
        assert_eq!(permutations(1), [[0]]);
        assert_eq!(permutations(2), [[0, 1], [1, 0]]);
        let three = permutations(3);
        assert_eq!(three.len(), 6);
        let distinct: HashSet<_> = three.iter().collect();
        assert_eq!(distinct.len(), 6);
    }

    #[test]
    fn two_accounts_have_three_distinct_orders() {
        let (orders, truncated) = candidates(&with_accounts(2));
        // Together (current), A first, B first. "A last" is "B first".
        assert_eq!(orders, [vec![1, 1], vec![1, 2], vec![2, 1]]);
        assert!(!truncated);
    }

    #[test]
    fn variants_are_distinct_and_capped_at_13() {
        for count in 3..=8 {
            let (orders, truncated) = candidates(&with_accounts(count));
            let distinct: HashSet<&Order> = orders.iter().collect();
            assert_eq!(distinct.len(), orders.len(), "{count} accounts");
            assert!(orders.len() <= MAX_OPTIONS, "{count} accounts");
            // The current order always comes first.
            assert_eq!(orders[0], vec![1; count]);
            // 3 accounts: all 13 orders fit. 6 accounts: together + 6 first
            // + 6 last = 13, all kept. 7: 15, two cut.
            let expected_truncated = count >= 7;
            assert_eq!(truncated, expected_truncated, "{count} accounts");
        }
    }

    #[test]
    fn three_accounts_compare_every_order() {
        let (orders, truncated) = candidates(&basic());
        // 1 all together + 6 "one group of 2 and one single" + 6 strict.
        assert_eq!(orders.len(), 13);
        assert!(!truncated);
    }

    #[test]
    fn descriptions_use_account_names() {
        let plan = basic(); // 401(k), Roth IRA, brokerage
        assert_eq!(
            describe(&[2, 3, 1], &plan),
            "Example brokerage first, then Example 401(k), then Example Roth IRA"
        );
        assert_eq!(
            describe(&[2, 2, 1], &plan),
            "Example brokerage first, then Example 401(k) and Example Roth IRA together"
        );
        assert_eq!(
            describe(&[1, 1, 1], &plan),
            "all accounts together, in proportion to their balances"
        );
    }

    #[tokio::test]
    async fn options_are_ranked_by_how_long_the_money_lasts_then_ending_balance() {
        // Accounts 0 and 1. Outcomes by priorities:
        // - together (current, [2, 2]): short from year 30
        // - account 0 first ([1, 2]):   lasts, ends with $500
        // - account 1 first ([2, 1]):   lasts, ends with $900
        let fake = Arc::new(FakeProjector::from_fn(|plan| {
            let priorities: Vec<i32> = plan.items.iter().map(|i| i.withdrawal_priority).collect();
            Ok(match priorities.as_slice() {
                [1, 2] => outcome(None, 500),
                [2, 1] => outcome(None, 900),
                _ => outcome(Some(30), 0),
            })
        }));

        let result = withdrawal_order_options(Arc::clone(&fake), &with_accounts(2))
            .await
            .unwrap();

        let ranked: Vec<(Option<usize>, &str, bool)> = result
            .options
            .iter()
            .map(|o| (o.rank, o.description.as_str(), o.is_current))
            .collect();
        assert_eq!(
            ranked,
            [
                (Some(1), "Account 1 first, then Account 0", false),
                (Some(2), "Account 0 first, then Account 1", false),
                (
                    Some(3),
                    "all accounts together, in proportion to their balances",
                    true
                ),
            ]
        );
        // Changes are against the current order.
        let Outcome::Projected {
            change_from_current,
            ..
        } = &result.options[0].outcome
        else {
            panic!("the best option should be projected");
        };
        assert_eq!(change_from_current.years_short, -11);
        assert_eq!(change_from_current.ending_balance, 900);
        // Priorities are ready to use with `set_item`.
        let priorities: Vec<(&str, i32)> = result.options[0]
            .priorities
            .iter()
            .map(|p| (p.item_id.as_str(), p.withdrawal_priority))
            .collect();
        assert_eq!(priorities, [("item-0", 2), ("item-1", 1)]);
        assert_eq!(fake.received().len(), 3);
    }

    #[tokio::test]
    async fn ties_keep_the_current_order_first() {
        let fake = Arc::new(FakeProjector::from_fn(|_| Ok(outcome(Some(30), 0))));
        let result = withdrawal_order_options(fake, &with_accounts(2))
            .await
            .unwrap();
        assert!(result.options[0].is_current);
        assert_eq!(result.options[0].rank, Some(1));
    }

    #[tokio::test]
    async fn the_current_order_runs_on_the_plan_as_given() {
        let fake = Arc::new(FakeProjector::from_fn(|_| Ok(outcome(None, 1))));
        let plan = basic();

        withdrawal_order_options(Arc::clone(&fake), &plan)
            .await
            .unwrap();

        // The first plan sent is the original, priorities untouched.
        let received = fake.received();
        assert!(received.contains(&plan));
        assert_eq!(received.len(), MAX_OPTIONS);
    }

    #[tokio::test]
    async fn a_single_account_plan_returns_one_option_with_a_note() {
        let fake = Arc::new(FakeProjector::from_fn(|_| Ok(outcome(None, 1))));

        let result = withdrawal_order_options(Arc::clone(&fake), &with_accounts(1))
            .await
            .unwrap();

        assert_eq!(result.options.len(), 1);
        assert!(result.options[0].is_current);
        assert!(result.note.as_deref().unwrap().contains("only one account"));
        assert_eq!(
            result.options[0].description,
            "Account 0 (the only account)"
        );
        assert_eq!(fake.received().len(), 1);
    }

    #[tokio::test]
    async fn failed_options_come_last_and_a_failed_current_fails_the_call() {
        let fake = Arc::new(FakeProjector::from_fn(|plan| {
            if plan.items[0].withdrawal_priority == 1 {
                Err(Error::Api {
                    problem: "timed out".to_string(),
                })
            } else {
                Ok(outcome(None, 1))
            }
        }));
        let result = withdrawal_order_options(fake, &with_accounts(2))
            .await
            .unwrap();
        let last = result.options.last().unwrap();
        assert_eq!(last.rank, None);
        assert!(matches!(&last.outcome, Outcome::Failed { error } if error.contains("timed out")));

        let fake = Arc::new(FakeProjector::scripted(vec![Err(Error::Auth)]));
        // With a scripted fake, results arrive in call order; only the first
        // call is scripted, so make it the only call: one account.
        let result = withdrawal_order_options(fake, &with_accounts(1)).await;
        assert!(matches!(result, Err(Error::Auth)), "{result:?}");
    }
}
