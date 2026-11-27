//! Deterministic selection only. The exclusive installer owns deletion.

use std::collections::{BTreeMap, BTreeSet};

use super::package;

const MAX_VERSIONS: usize = 64;
const RETAINED: usize = 4;

pub(super) fn obsolete(
    current: &str,
    previous: Option<&str>,
    versions: &[String],
    activations: &[String],
) -> Result<Vec<String>, &'static str> {
    if versions.len() > MAX_VERSIONS {
        return Err("installation exceeds the version inventory bound");
    }
    if !package::commit(current)
        || previous.is_some_and(|value| !package::commit(value) || value == current)
    {
        return Err("installation protected identities are invalid");
    }
    let mut complete = BTreeSet::new();
    for version in versions {
        if !package::commit(version) || !complete.insert(version.as_str()) {
            return Err("installation version inventory is invalid or duplicated");
        }
    }
    if !complete.contains(current) || previous.is_some_and(|value| !complete.contains(value)) {
        return Err("installation protected version is missing");
    }
    let mut kept = BTreeSet::from([current]);
    if let Some(previous) = previous {
        kept.insert(previous);
    }
    let mut recency = BTreeMap::new();
    for (position, activation) in activations.iter().enumerate() {
        if !package::commit(activation) {
            return Err("installation activation history has an invalid build");
        }
        recency.insert(activation.as_str(), position);
    }
    let mut ranked: Vec<&str> = complete.iter().copied().collect();
    // History position is activation order, including a revert to an older
    // build. Missing entries sort last, by commit only as an explicit fallback.
    ranked.sort_by(|left, right| {
        recency
            .get(right)
            .cmp(&recency.get(left))
            .then_with(|| right.cmp(left))
    });
    for version in ranked {
        if kept.len() == RETAINED {
            break;
        }
        kept.insert(version);
    }
    Ok(complete
        .difference(&kept)
        .map(|value| (*value).to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{obsolete, MAX_VERSIONS};

    fn versions(count: usize) -> Vec<String> {
        (0..count).map(|value| format!("{value:040x}")).collect()
    }

    #[test]
    fn low_sorting_current_and_previous_remain_protected() -> Result<(), &'static str> {
        let inventory = versions(6);
        assert_eq!(
            obsolete(&inventory[0], Some(&inventory[1]), &inventory, &[])?,
            vec![inventory[2].clone(), inventory[3].clone()]
        );
        Ok(())
    }

    #[test]
    fn previous_is_optional_and_the_other_three_use_descending_identity() -> Result<(), &'static str>
    {
        let inventory = versions(6);
        assert_eq!(
            obsolete(&inventory[0], None, &inventory, &[])?,
            vec![inventory[1].clone(), inventory[2].clone()]
        );
        Ok(())
    }

    #[test]
    fn fewer_than_five_complete_versions_need_no_deletion() -> Result<(), &'static str> {
        for count in 1..=4 {
            let inventory = versions(count);
            assert!(obsolete(&inventory[0], None, &inventory, &[])?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn missing_or_equal_protected_versions_refuse() {
        let inventory = versions(4);
        let outside = format!("{:040x}", 10);
        assert!(obsolete(&outside, None, &inventory, &[]).is_err());
        assert!(obsolete(&inventory[0], Some(&outside), &inventory, &[]).is_err());
        assert!(obsolete(&inventory[0], Some(&inventory[0]), &inventory, &[]).is_err());
    }

    #[test]
    fn invalid_and_duplicated_inventory_refuse() {
        let inventory = versions(4);
        let mut duplicated = inventory.clone();
        duplicated.push(inventory[0].clone());
        assert!(obsolete(&inventory[0], None, &duplicated, &[]).is_err());
        let mut invalid = inventory.clone();
        invalid[1] = "../outside".into();
        assert!(obsolete(&inventory[0], None, &invalid, &[]).is_err());
        assert!(obsolete("../outside", None, &inventory, &[]).is_err());
        assert!(obsolete(&inventory[0], Some("../outside"), &inventory, &[]).is_err());
    }

    #[test]
    fn over_bound_inventory_is_refused_before_selection() {
        let inventory = versions(MAX_VERSIONS + 1);
        assert!(obsolete(&inventory[0], None, &inventory, &[]).is_err());
    }

    #[test]
    fn inventory_order_never_changes_the_retention_result() -> Result<(), &'static str> {
        let mut inventory = versions(6);
        let current = inventory[0].clone();
        let previous = inventory[1].clone();
        let expected = obsolete(&current, Some(&previous), &inventory, &[])?;
        inventory.reverse();
        assert_eq!(
            obsolete(&current, Some(&previous), &inventory, &[])?,
            expected
        );
        Ok(())
    }

    #[test]
    fn activation_recency_beats_commit_order() -> Result<(), &'static str> {
        let inventory = versions(6);
        let activations = vec![
            inventory[5].clone(),
            inventory[4].clone(),
            inventory[3].clone(),
            inventory[2].clone(),
        ];
        assert_eq!(
            obsolete(&inventory[0], Some(&inventory[1]), &inventory, &activations)?,
            vec![inventory[4].clone(), inventory[5].clone()]
        );
        Ok(())
    }

    #[test]
    fn reverting_to_an_old_commit_makes_it_recent() -> Result<(), &'static str> {
        let inventory = versions(6);
        let activations = vec![
            inventory[1].clone(),
            inventory[2].clone(),
            inventory[3].clone(),
            inventory[4].clone(),
            inventory[5].clone(),
            inventory[1].clone(),
        ];
        assert_eq!(
            obsolete(&inventory[0], Some(&inventory[5]), &inventory, &activations)?,
            vec![inventory[2].clone(), inventory[3].clone()]
        );
        Ok(())
    }

    #[test]
    fn recorded_activations_rank_ahead_of_unrecorded_builds() -> Result<(), &'static str> {
        let inventory = versions(6);
        assert_eq!(
            obsolete(&inventory[0], None, &inventory, &[inventory[1].clone()])?,
            vec![inventory[2].clone(), inventory[3].clone()]
        );
        assert!(obsolete(&inventory[0], None, &inventory, &["invalid".into()]).is_err());
        Ok(())
    }
}
