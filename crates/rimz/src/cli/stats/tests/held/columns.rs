use super::*;

fn lines(active: Window) -> Vec<String> {
    lines_at(active, None)
}

fn lines_at(active: Window, rows: Option<usize>) -> Vec<String> {
    let (stats, assists) = fixture();
    strip_ansi(&frame(&stats, &assists, active, 119, rows).text)
        .lines()
        .map(|line| line.strip_prefix("   ").unwrap_or(line).to_owned())
        .collect()
}

#[test]
fn spend_columns_match_the_mock() {
    let month = lines(Window::Month);
    for expected in [
        "Models                                                                                  HIT       TOK/S  TOKENS",
        "█ Opus 5.5       $9,371  ━━━━━━━━━━━━━━━━━━──────────────────────────────  38%          96%          85   15.4B",
        "Agents             $904                                   Subagents        $1,681",
        "● brainstormer     $266  ━━━━━━━─────────────────  29%    ● implementer    $1,060  ━━━━━━━━━━━━━━━─────────  63%",
        "Teams            $3,148                                                          AGENT-TIME    MESSAGES  TOKENS",
        "● recon          $2,621  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━────────  83%       18h10m          34    4.5B",
    ] {
        assert!(
            month.iter().any(|line| line == expected),
            "missing mock row: {expected}\n{}",
            month.join("\n")
        );
    }
    let today = lines(Window::Today);
    assert!(today.iter().any(|line| line == "█ Opus 5.5         $688  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━────────────────  67%          97%          88    1.3B"));
}

#[test]
fn provider_and_role_columns_match_the_mock() {
    let month = lines(Window::Month);
    assert!(month.iter().any(|line| line == "█ Claude     $13,232   54%   96% hit     877 sessions     █ Teams     $3,148  13%    ▓ Subagents $1,681   7%"), "{}", month.join("\n"));
    assert!(month.iter().any(|line| line == "█ Codex      $11,340   46%   97% hit   5,995 sessions     ▒ Agents      $904   4%    ░ No role  $18,839  77%"));
    assert!(
        month
            .iter()
            .any(|line| line == "█ Pi           $0.04   <1%                 1 session")
    );
    let today = lines(Window::Today);
    assert!(
        today
            .iter()
            .any(|line| line.starts_with("█ Claude        $691   67%   97% hit      42 sessions"))
    );
    assert!(
        today
            .iter()
            .any(|line| line.starts_with("█ Codex         $334   33%   97% hit     348 sessions"))
    );
}

#[test]
fn heatmap_columns_match_the_mock() {
    let month = lines(Window::Month);
    assert!(
        month.iter().any(|line| line.starts_with("   Mon ")),
        "weekday label and cells must retain the mock gutter"
    );
    let key = month
        .iter()
        .find(|line| line.contains("30 of last"))
        .unwrap();
    assert_eq!(display_width(&key[..key.find("30 of last").unwrap()]), 40);
    assert!(key.ends_with("30 of last 30 days active · streak 65 days · busiest Oct 22"));
    let months = month
        .iter()
        .find(|line| line.contains("Nov") && line.contains("Dec"))
        .unwrap();
    assert_eq!(display_width(&months[..months.find("Nov").unwrap()]), 9);
}

#[test]
fn typical_line_matches_the_mock_row() {
    for (rows, height) in [(None, 12), (Some(40), 6)] {
        let month = lines_at(Window::Month, rows);
        let start = month
            .iter()
            .position(|line| line.starts_with("Spend per day"))
            .unwrap();
        let plot = &month[start + 1..start + 1 + height];
        assert!(plot[0].contains("$2,000"), "{}", month.join("\n"));
        let half = plot
            .iter()
            .position(|line| line.contains("$1,000"))
            .unwrap();
        let typical = plot
            .iter()
            .position(|line| line.contains("typical $785"))
            .unwrap();
        assert_eq!(
            typical,
            half + 1,
            "typical belongs just below the half-ceiling label at height {height}"
        );
    }
}

#[test]
fn chart_labels_match_the_mock_columns() {
    let month = lines(Window::Month);
    let axis = month.iter().find(|line| line.contains("└─")).unwrap();
    assert_eq!(display_width(&axis[..axis.find("today").unwrap()]), 91);
    let typical = month
        .iter()
        .find(|line| line.contains("typical $785"))
        .unwrap();
    assert_eq!(
        display_width(&typical[..typical.find("typical").unwrap()]),
        101
    );
    assert!(typical.ends_with("██┄ typical $785"), "{typical}");
    assert_eq!(display_width(typical), 113);
}
