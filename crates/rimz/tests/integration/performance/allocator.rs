//! The shipped jemalloc configuration: the exported `malloc_conf` must reach
//! jemalloc so idle processes purge dirty pages and extents stay off huge pages.

use assert_cmd::prelude::*;
use predicates::prelude::PredicateBooleanExt;

use crate::common::Env;

#[test]
fn jemalloc_reads_the_shipped_config() {
    let env = Env::new();
    env.rimz()
        .arg("--version")
        .env("MALLOC_CONF", "stats_print:true")
        .assert()
        .success()
        .stderr(predicates::str::contains("opt.dirty_decay_ms: 0"))
        .stderr(
            predicates::str::contains("opt.thp: \"never\"")
                .or(predicates::str::contains("opt.thp: \"not supported\"")),
        );
}
