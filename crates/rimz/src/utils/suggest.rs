//! Nearby names for command-line corrections.

/// Pick the nearest name within two edits, breaking ties lexicographically.
pub fn nearest_name<'a>(input: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names
        .into_iter()
        .map(|name| {
            let mut row: Vec<_> = (0..=name.chars().count()).collect();
            for (i, left) in input.chars().enumerate() {
                let mut diagonal = row[0];
                row[0] = i + 1;
                for (j, right) in name.chars().enumerate() {
                    let above = row[j + 1];
                    row[j + 1] = (diagonal + usize::from(left != right))
                        .min(above + 1)
                        .min(row[j] + 1);
                    diagonal = above;
                }
            }
            (row[name.chars().count()], name)
        })
        .min()
        .filter(|(distance, _)| *distance <= 2)
        .map(|(_, name)| name)
}
