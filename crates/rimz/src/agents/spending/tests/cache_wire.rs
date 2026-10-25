use super::*;
use serde::Deserialize;
use serde_json::value::RawValue;

const V28: &str = concat!(
    r#"{"version":28,"files":{"/claude.jsonl":{"s":{"mtime_secs":1750000000,"len":4096},"#,
    r#""c":{"o":4096,"a":123,"s":{"current_message":{"message_id":"msg-1","start_ms":1234},"#,
    r#""latest_user_ms":1200,"pricing_fingerprint":"prices-v1"}},"p":"/workspace","e":["#,
    r#"{"t":1750000000,"u":0.125,"i":100,"o":200,"w":30,"r":40,"n":{"Read":2},"#,
    r#""m":"msg-\"1","q":"req-1","l":"claude-opus-4-8"},"#,
    r#"{"t":1750000001,"u":0.25,"n":{"Bash":1,"bash":2},"#,
    r#""m":"msg-2","q":"req-2","s":true,"l":"claude-opus-4-8"},"#,
    r#"{"t":1750000002,"u":0.5,"o":300,"g":{"m":1000,"o":300},"n":{"検索":1},"#,
    r#""m":"msg-3","q":"req-3","v":true,"f":true,"l":"モデル"},"#,
    r#"{"t":1700000000,"u":1.0,"n":{"Read":3},"h":"thread-old","#,
    r#""l":"claude-opus-4-8","d":true}],"u":{"unknown-model":1749999999}},"#,
    r#""/codex.jsonl":{"s":{"mtime_secs":1750000003,"mtime_nanos":123,"len":8192},"#,
    r#""c":{"o":8192,"s":{"current_model":"gpt-5-codex","cwd":"/workspace","fast":true,"#,
    r#""pending_tool_calls":{"Bash":2,"Read":1},"#,
    r#""previous_totals":{"cached_input_tokens":10,"input_tokens":100,"output_tokens":20},"#,
    r#""replay_checked":true,"response_boundary":"AwaitingInput","skipping_replay":false}},"#,
    r#""e":[{"t":1750000003,"u":0.03125,"i":90,"o":20,"r":10,"n":{"Bash":2,"Read":1},"#,
    r#""k":"codex:event-1","l":"gpt-5-codex"},"#,
    r#"{"t":1750000004,"u":0.0625,"k":"codex:event-2","l":"gpt-5-codex"}]},"#,
    r#""/plugin.jsonl":{"s":{"mtime_secs":1750000005,"len":12},"#,
    r#""c":{"o":12,"s":["opaque",{"a":1,"z":2}]},"#,
    r#""e":[{"t":1750000005,"u":0.0,"h":"plugin-thread"}]}}}"#,
    "\n",
);

#[derive(Deserialize)]
struct RawCache {
    files: BTreeMap<String, Box<RawValue>>,
}

#[test]
fn v28_cache_preserves_per_file_and_single_file_bytes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("spending.json");
    std::fs::write(&path, V28).unwrap();
    let cache = read_spending_cache(&path);
    assert_eq!(cache.version, 28);
    let raw: RawCache = serde_json::from_str(V28).unwrap();
    assert_eq!(cache.files.len(), raw.files.len());
    for (key, value) in &raw.files {
        assert_eq!(
            serde_json::to_string(&cache.files[key]).unwrap(),
            value.get()
        );
        let key_json = serde_json::to_string(key).unwrap();
        let single = format!(
            "{{\"version\":28,\"files\":{{{key_json}:{}}}}}\n",
            value.get()
        );
        std::fs::write(&path, &single).unwrap();
        let decoded = read_spending_cache(&path);
        assert!(write_spending_cache(&path, &decoded));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), single);
    }
    let tools: Vec<_> = raw
        .files
        .keys()
        .flat_map(|key| &cache.files[key].entries)
        .map(|entry| serde_json::to_string(&entry.tool_calls).unwrap())
        .collect();
    assert_eq!(
        tools,
        [
            r#"{"Read":2}"#,
            r#"{"Bash":1,"bash":2}"#,
            r#"{"検索":1}"#,
            r#"{"Read":3}"#,
            r#"{"Bash":2,"Read":1}"#,
            "{}",
            "{}",
        ]
    );
}
