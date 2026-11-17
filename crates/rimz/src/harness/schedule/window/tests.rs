use super::*;

fn publish_windows(runtime: &RuntimePaths, kind: &str, windows: Vec<RateLimitWindow>) {
    publish_windows_for(
        runtime,
        crate::ids::LoginKey::default_for(AgentKind::new_unchecked(kind)),
        windows,
    );
}

fn publish_windows_for(
    runtime: &RuntimePaths,
    key: crate::ids::LoginKey,
    windows: Vec<RateLimitWindow>,
) {
    crate::disk::atomic::write_temp_then_rename_cache(
        &crate::agents::account::RateLimitsCache::shared_path(runtime),
        &crate::agents::RateLimitsCache {
            entries: std::collections::BTreeMap::from([(
                key.into(),
                crate::agents::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits { windows },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn window_triggers_refuse_at_add_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let window = |used, resets_in: i64, span: WindowSpan| RateLimitWindow {
        used_percentage: used,
        resets_at: Some(now + jiff::SignedDuration::from_secs(resets_in)),
        duration_mins: Some(span.minutes()),
        ..RateLimitWindow::default()
    };
    let native = crate::agents::RoomLoginSet::new(
        Some(Default::default()),
        None,
        std::collections::BTreeMap::new(),
    );
    let at_add = |kind: Option<&str>, span, logins: &crate::agents::RoomLoginSet| {
        let key = kind.and_then(|kind| logins.default_key(kind));
        window_at_add(kind, span, key.as_ref(), dir.path(), &runtime, now)
    };
    let five = WindowSpan::FiveHour;
    let seven = WindowSpan::SevenDay;
    assert_eq!(at_add(None, five, &native), Err(WindowRefusal::NoProvider));
    let refusal = at_add(Some("qwen"), five, &native).unwrap_err();
    assert!(matches!(refusal, WindowRefusal::ManagedAccount { .. }));
    assert!(
        refusal.to_string().contains("do not support qwen"),
        "{refusal}"
    );
    let unresolvable = crate::agents::RoomLoginSet::new(
        Some(
            std::collections::BTreeMap::from([(
                AgentKind::new_unchecked("claude"),
                "work".parse().unwrap(),
            )])
            .into(),
        ),
        None,
        std::collections::BTreeMap::new(),
    );
    let refusal = at_add(Some("claude"), five, &unresolvable).unwrap_err();
    assert!(
        refusal.to_string().ends_with("run `rimz accounts list`"),
        "{refusal}"
    );
    let refusal = at_add(Some("claude"), five, &native).unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "no current claude 5h window reading; open the room's sidebar or run `rimz accounts --refresh`"
    );
    // A model refusal record is no window reading.
    std::fs::write(
        runtime.shared_model_refusals_path(),
        r#"{"logins":{"claude@default":{"fable 5.2":{"refused_at":"2033-05-18T03:00:00Z"}}}}"#,
    )
    .unwrap();
    assert!(matches!(
        at_add(Some("claude"), five, &native),
        Err(WindowRefusal::NoReading { .. })
    ));
    publish_windows(&runtime, "claude", vec![window(Some(30), -60, five)]);
    assert!(matches!(
        at_add(Some("claude"), five, &native),
        Err(WindowRefusal::NoReading { .. })
    ));
    publish_windows(&runtime, "claude", vec![window(Some(30), 3_600, five)]);
    assert_eq!(
        at_add(Some("claude"), seven, &native)
            .unwrap_err()
            .to_string(),
        "claude has no 7d window"
    );
    let started = at_add(Some("claude"), five, &native).unwrap();
    assert_eq!(started.kind.as_str(), "claude");
    assert_eq!(started.window, window(Some(30), 3_600, five));
    let lifted = RateLimitWindow {
        lifted: true,
        resets_at: None,
        used_percentage: None,
        ..window(None, 0, five)
    };
    publish_windows(&runtime, "codex", vec![lifted.clone()]);
    assert_eq!(at_add(Some("codex"), five, &native).unwrap().window, lifted);
    let lifted_past_reset = RateLimitWindow {
        lifted: true,
        ..window(Some(30), -60, five)
    };
    publish_windows(&runtime, "codex", vec![lifted_past_reset.clone()]);
    assert_eq!(
        at_add(Some("codex"), five, &native).unwrap().window,
        lifted_past_reset,
        "a lifted window passes the reading check whatever its stale reset"
    );

    // A pinned task reads its own account's stored window, not the room's.
    let work =
        crate::ids::LoginKey::new(AgentKind::new_unchecked("claude"), "work".parse().unwrap());
    publish_windows_for(&runtime, work.clone(), vec![window(Some(55), 3_600, five)]);
    assert!(matches!(
        at_add(Some("claude"), five, &native),
        Err(WindowRefusal::NoReading { .. })
    ));
    assert_eq!(
        window_at_add(Some("claude"), five, Some(&work), dir.path(), &runtime, now)
            .unwrap()
            .window,
        window(Some(55), 3_600, five)
    );
}

#[test]
fn after_reset_fires_at_the_reset_or_now_and_refuses_what_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let native = crate::ids::LoginKey::default_for(AgentKind::new_unchecked("claude"));
    let five = WindowSpan::FiveHour;
    let seven = WindowSpan::SevenDay;
    let window = |used, resets_in: Option<i64>, span: WindowSpan| RateLimitWindow {
        used_percentage: used,
        resets_at: resets_in.map(|secs| now + jiff::SignedDuration::from_secs(secs)),
        duration_mins: Some(span.minutes()),
        ..RateLimitWindow::default()
    };
    let resolve = |span| {
        after_reset(
            span,
            Some("claude"),
            Some(&native),
            dir.path(),
            &runtime,
            now,
        )
    };
    assert_eq!(
        after_reset(five, None, None, dir.path(), &runtime, now),
        Err(WindowRefusal::NoProvider)
    );
    let week = window(Some(10), Some(86_400), seven);
    publish_windows(
        &runtime,
        "claude",
        vec![window(Some(30), Some(3_600), five), week.clone()],
    );
    let started = resolve(five).unwrap();
    assert_eq!(
        started.fire_at,
        now + jiff::SignedDuration::from_secs(3_600)
    );
    assert_eq!((started.left, started.started), (Some(70), true));
    assert_eq!(resolve(seven).unwrap().fire_at, week.resets_at.unwrap());

    let fresh = window(Some(0), Some(i64::from(five.minutes()) * 60), five);
    publish_windows(&runtime, "claude", vec![fresh.clone(), week.clone()]);
    let not_started = resolve(five).unwrap();
    assert_eq!((not_started.fire_at, not_started.started), (now, false));
    assert_eq!(not_started.resets_at, fresh.resets_at.unwrap());

    let lifted = RateLimitWindow {
        lifted: true,
        ..window(None, None, five)
    };
    publish_windows(&runtime, "claude", vec![lifted, week.clone()]);
    let refusal = resolve(five).unwrap_err();
    assert!(
        refusal.to_string().contains("--after-reset 7d"),
        "{refusal}"
    );
    publish_windows(&runtime, "claude", vec![window(Some(30), None, five)]);
    assert!(matches!(
        resolve(five),
        Err(WindowRefusal::NoReading { .. })
    ));
}

#[test]
fn window_conditions_record_the_provider_their_terms_read() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let provider = |when: &str, kind: Option<&str>| {
        let key =
            kind.map(|kind| crate::ids::LoginKey::default_for(AgentKind::new_unchecked(kind)));
        window_condition_provider(
            &[when.to_owned()],
            kind,
            key.as_ref(),
            dir.path(),
            &runtime,
            now,
        )
    };
    assert_eq!(provider("ci=passed", None), Ok(None));
    assert_eq!(provider("ci=passed", Some("claude")), Ok(None));
    assert_eq!(provider("ci=", None), Ok(None));
    assert_eq!(
        provider("ci=passed || window.5h.left>=40", None),
        Err(WindowRefusal::NoProvider)
    );
    assert!(matches!(
        provider("window.5h.left>=40", Some("claude")),
        Err(WindowRefusal::NoReading { .. })
    ));
    publish_windows(
        &runtime,
        "claude",
        vec![RateLimitWindow {
            used_percentage: Some(92),
            resets_at: Some(now + jiff::SignedDuration::from_secs(3_600)),
            duration_mins: Some(WindowSpan::FiveHour.minutes()),
            ..RateLimitWindow::default()
        }],
    );
    assert_eq!(
        provider("window.5h.left>=40", Some("claude")),
        Ok(Some(AgentKind::new_unchecked("claude")))
    );
    assert_eq!(
        provider("window.5h.left>=40 && window.7d.left>=10", Some("claude"))
            .unwrap_err()
            .to_string(),
        "claude has no 7d window"
    );
}
