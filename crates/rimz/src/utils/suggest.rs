//! Nearby names for command-line corrections.

/// Pick up to three names at the nearest distance within two edits, in input order.
pub fn nearest_names<'a>(input: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let scored = names
        .into_iter()
        .map(|name| (distance(input, name), name))
        .collect::<Vec<_>>();
    let nearest = scored
        .iter()
        .map(|(distance, _)| *distance)
        .min()
        .unwrap_or(3);
    scored
        .into_iter()
        .filter(|(distance, _)| *distance == nearest && *distance <= 2)
        .take(3)
        .map(|(_, name)| name)
        .collect()
}

/// Pick the nearest name within two edits, breaking ties lexicographically.
pub fn nearest_name<'a>(input: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names
        .into_iter()
        .map(|name| (distance(input, name), name))
        .min()
        .filter(|(distance, _)| *distance <= 2)
        .map(|(_, name)| name)
}

fn distance(input: &str, name: &str) -> usize {
    let left = input.chars().collect::<Vec<_>>();
    let right = name.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut before_previous = previous.clone();
    let mut row = previous.clone();
    for (i, left_char) in left.iter().enumerate() {
        row[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            row[j + 1] = (previous[j] + usize::from(left_char != right_char))
                .min(previous[j + 1] + 1)
                .min(row[j] + 1);
            if i > 0 && j > 0 && *left_char == right[j - 1] && left[i - 1] == *right_char {
                row[j + 1] = row[j + 1].min(before_previous[j - 1] + 1);
            }
        }
        std::mem::swap(&mut before_previous, &mut previous);
        std::mem::swap(&mut previous, &mut row);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpositions_are_one_edit() {
        assert_eq!(nearest_names("lsit", ["wait", "list"]), ["list"]);
        assert_eq!(nearest_names("shwo", ["stop", "show"]), ["show"]);
        assert_eq!(nearest_name("lsit", ["wait", "list"]), Some("list"));
    }

    #[test]
    fn nearest_tier_preserves_order_and_caps_at_three() {
        assert_eq!(
            nearest_names("per", ["perl", "peer", "perf", "pear", "pair"]),
            ["perl", "peer", "perf"]
        );
        assert_eq!(
            nearest_names("per", ["pair", "perf", "peer"]),
            ["perf", "peer"]
        );
    }

    #[test]
    fn distant_names_are_not_suggested() {
        assert!(nearest_names("xyz", ["perf", "peer"]).is_empty());
        assert_eq!(nearest_name("xyz", ["perf", "peer"]), None);
    }

    #[test]
    fn single_name_breaks_ties_alphabetically() {
        assert_eq!(
            nearest_name("per", ["perl", "perf", "peer", "per", "pet"]),
            Some("per")
        );
        assert_eq!(
            nearest_name("per", ["pet", "perl", "perf", "peer"]),
            Some("peer")
        );
    }
}
