use super::*;

fn bit_exact(scoped: &ScopedSpending) -> String {
    let ScopedSpending {
        tally,
        headline_cutoff_secs,
        live_baselines,
        day,
        day_cutoff_secs,
    } = scoped;
    let mut lines = vec![format!("cutoffs: {headline_cutoff_secs} {day_cutoff_secs}")];
    for (name, window) in [
        ("headline", &tally.headline),
        ("day", &tally.day),
        ("week", &tally.week),
        ("month", &tally.month),
        ("year", &tally.year),
        ("local day", day),
    ] {
        let SpendWindow {
            usd,
            tokens,
            input,
            output,
            timed_ms,
            timed_output,
            cache_write,
            cache_read,
            tool_calls,
            tools,
            sessions,
        } = window;
        lines.push(format!(
            "{name}: {:016x} {tokens} {input} {output} {timed_ms} {timed_output} \
             {cache_write} {cache_read} {tool_calls} {tools:?} {sessions}",
            usd.to_bits(),
        ));
    }
    for (session, usd) in live_baselines {
        lines.push(format!("{session}: {:016x}", usd.to_bits()));
    }
    lines.join("\n")
}

#[test]
fn room_spend_slice_matches_trunk_bit_for_bit() {
    const DAY: u64 = 86_400;
    const HOUR: u64 = 3_600;
    let project = Path::new("/repo/project");
    let worktree = Path::new("/elsewhere/worktree");
    let home = Path::new("/repo/project-worktrees");
    let scope = SpendScope::for_workspace(Some(project), &[worktree.into()], Some(home));
    let row = |age, cost, session: &str| cached_entry(NOW_SECS - age, cost, session);
    let keyed = |id: &str, cost, sidechain| CachedEntry {
        message_id: Some(id.into()),
        request_id: Some(if sidechain { "replay" } else { "request" }.into()),
        is_sidechain: sidechain,
        ..row(
            60,
            cost,
            match id {
                "a" | "b" | "c" => "rounding",
                _ => id,
            },
        )
    };
    let grouped = |age, cost| CachedEntry {
        thread_id: None,
        ..row(age, cost, "")
    };
    let mut cache = SpendingDiskCache {
        version: SPENDING_CACHE_VERSION,
        ..Default::default()
    };
    let mut files = Vec::new();
    for (path, origin, entries) in [
        (
            "/history/main.jsonl",
            Some(project),
            vec![
                keyed("a", 0.1, false),
                keyed("b", 0.2, false),
                keyed("c", 0.3, false),
                keyed("outside-wins", 90.0, false),
                keyed("inside-wins", 0.4, false),
                keyed("sidechain", 0.5, false),
                row(2 * HOUR, 0.6, "burst"),
                row(4 * HOUR, 0.7, "burst"),
                row(6 * HOUR, 0.8, "burst"),
                row(20 * HOUR, 0.9, "yesterday"),
                row(3 * DAY, 1.1, "week"),
                row(15 * DAY, 1.2, "month"),
                row(100 * DAY, 1.3, "burst"),
                row(RAW_RETAIN_SECS, 1.4, "expired"),
                row(WIDEST_SPEND_WINDOW_SECS + DAY, 80.0, "burst"),
                CachedEntry {
                    rolled: true,
                    ..row(40 * DAY, 1.5, "rolled")
                },
            ],
        ),
        (
            "/history/projects/project/11111111-1111-4111-8111-111111111111.jsonl",
            Some(worktree),
            vec![grouped(30, 1.6)],
        ),
        (
            "/history/projects/project/11111111-1111-4111-8111-111111111111/subagents/agent-child.jsonl",
            Some(Path::new("/repo/project-worktrees/deleted")),
            vec![grouped(20, 1.7), keyed("sidechain", 70.0, true)],
        ),
        (
            "/history/normalized.jsonl",
            Some(Path::new("/repo/project/child/../src")),
            vec![row(10, 1.8, "normalized")],
        ),
        (
            "/history/outside.jsonl",
            Some(Path::new("/other")),
            vec![
                keyed("outside-wins", 60.0, false),
                keyed("inside-wins", 50.0, false),
                row(1, 40.0, "outside"),
            ],
        ),
        (
            "/history/unknown.jsonl",
            None,
            vec![row(1, 30.0, "unknown")],
        ),
    ] {
        let path = PathBuf::from(path);
        let (key, mut file) = cached_file(&path, entries);
        file.origin_path = origin.map(Path::to_path_buf);
        if path == Path::new("/history/outside.jsonl") {
            file.entries[0].input += 1;
            file.entries[1].input -= 1;
        }
        cache.files.insert(key, file);
        files.push(spending_file(claude_adapter(), path));
    }
    let inputs = [
        (NOW_SECS - 7 * HOUR, Some(project)),
        (NOW_SECS - 2 * DAY, Some(worktree)),
        (NOW_SECS - 1, Some(Path::new("/other"))),
        (NOW_SECS, None),
    ]
    .map(|(at, origin)| user_input::UserInputRecord {
        at: jiff::Timestamp::from_second(at as i64).unwrap(),
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        origin: origin.map(Path::to_path_buf),
    });
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let dir = TempDir::new().unwrap();
    let cache_path = dir.path().join("spending.json");
    write_spending_cache(&cache_path, &cache);
    let mut walker = SpendingWalker::new();
    let mut pins = Vec::new();
    for mode in [
        SpendWindowMode::Session,
        SpendWindowMode::Trailing24h,
        SpendWindowMode::Today,
    ] {
        let spec = HeadlineSpec {
            mode,
            timezone: Some("America/New_York".into()),
        };
        let walk = aggregate_counted_rollups(
            &files,
            &cache,
            &counted,
            &contested_session_locations(&files, &cache),
            Some(&scope),
            HeadlineContext {
                user_inputs: &inputs,
                now_secs: NOW_SECS,
                spec: &spec,
            },
            None,
        )
        .workspace;
        let slice = compute_scoped_spending(&files, &cache, &inputs, &scope, NOW_SECS, &spec);
        let _ = walker.scoped_from_cache(&cache_path, &files, &inputs, &scope, NOW_SECS, &spec);
        let warm = walker.scoped_from_cache(&cache_path, &files, &inputs, &scope, NOW_SECS, &spec);
        assert!(warm.has_discovered_file);
        assert_eq!(bit_exact(&walk), bit_exact(&slice));
        assert_eq!(bit_exact(&walk), bit_exact(&warm.scoped));
        pins.push(format!("{mode:?}\n{}", bit_exact(&walk)));
    }
    insta::assert_snapshot!(pins.join("\n\n"), @"
    Session
    cutoffs: 1749974800 1749960000
    headline: 4021666666666666 165 110 55 0 0 0 0 0 {} 7
    day: 4021666666666666 165 110 55 0 0 0 0 0 {} 7
    week: 4025666666666666 195 130 65 0 0 0 0 0 {} 9
    month: 402a999999999999 225 150 75 0 0 0 0 0 {} 11
    year: 4030199999999999 255 170 85 0 0 0 0 0 {} 12
    local day: 4021666666666666 165 110 55 0 0 0 0 0 {} 6
    claude:11111111-1111-4111-8111-111111111111: 400a666666666666
    claude:burst: 400b333333333332
    claude:inside-wins: 3fd999999999999a
    claude:normalized: 3ffccccccccccccd
    claude:outside-wins: 4056800000000000
    claude:rounding: 3fe3333333333334
    claude:sidechain: 3fe0000000000000
    claude:week: 3ff199999999999a
    claude:yesterday: 3feccccccccccccd

    Trailing24h
    cutoffs: 1749913601 1749960000
    headline: 4023333333333333 180 120 60 0 0 0 0 0 {} 8
    day: 4021666666666666 165 110 55 0 0 0 0 0 {} 7
    week: 4025666666666666 195 130 65 0 0 0 0 0 {} 9
    month: 402a999999999999 225 150 75 0 0 0 0 0 {} 11
    year: 4030199999999999 255 170 85 0 0 0 0 0 {} 12
    local day: 4021666666666666 165 110 55 0 0 0 0 0 {} 6
    claude:11111111-1111-4111-8111-111111111111: 400a666666666666
    claude:burst: 400b333333333332
    claude:inside-wins: 3fd999999999999a
    claude:normalized: 3ffccccccccccccd
    claude:outside-wins: 4056800000000000
    claude:rounding: 3fe3333333333334
    claude:sidechain: 3fe0000000000000
    claude:week: 3ff199999999999a
    claude:yesterday: 3feccccccccccccd

    Today
    cutoffs: 1749960000 1749960000
    headline: 4021666666666666 165 110 55 0 0 0 0 0 {} 7
    day: 4021666666666666 165 110 55 0 0 0 0 0 {} 7
    week: 4025666666666666 195 130 65 0 0 0 0 0 {} 9
    month: 402a999999999999 225 150 75 0 0 0 0 0 {} 11
    year: 4030199999999999 255 170 85 0 0 0 0 0 {} 12
    local day: 4021666666666666 165 110 55 0 0 0 0 0 {} 6
    claude:11111111-1111-4111-8111-111111111111: 400a666666666666
    claude:burst: 400b333333333332
    claude:inside-wins: 3fd999999999999a
    claude:normalized: 3ffccccccccccccd
    claude:outside-wins: 4056800000000000
    claude:rounding: 3fe3333333333334
    claude:sidechain: 3fe0000000000000
    claude:week: 3ff199999999999a
    claude:yesterday: 3feccccccccccccd
    ");
}
