//! The shipped allocator configuration on Linux glibc: the binary's exported
//! `malloc_conf` must reach jemalloc, or idle processes keep dirty pages and,
//! on a kernel whose transparent huge page mode is `always`, small processes
//! fault in whole huge pages.

use assert_cmd::prelude::*;

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
        .stderr(predicates::str::contains("opt.thp: \"never\""));
}
