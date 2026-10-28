//! Process reads shared only within one synchronous pane production. Dropping
//! the scope, including on unwind, discards every value before a confirmation
//! candidate or another sampling pass can observe it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::Arc;

type FileReads = HashMap<(Option<u32>, &'static str), Option<Arc<[u8]>>>;

#[derive(Default)]
struct Reads {
    files: FileReads,
    cwds: HashMap<u32, Option<PathBuf>>,
    children: HashMap<u32, Vec<u32>>,
}

thread_local! {
    // Bounded by the processes visited in this production; never retained across passes.
    static READS: RefCell<Option<Reads>> = const { RefCell::new(None) };
}

pub(super) fn scope<T>(read: impl FnOnce() -> T) -> T {
    struct Reset(Option<Reads>);
    impl Drop for Reset {
        fn drop(&mut self) {
            READS.with(|reads| *reads.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(READS.with(|reads| reads.replace(Some(Reads::default()))));
    read()
}

fn cached<K: Eq + Hash, V: Clone>(
    key: K,
    cache: impl Fn(&mut Reads) -> &mut HashMap<K, V>,
    read: impl FnOnce() -> V,
) -> V {
    if let Some(value) = READS.with(|reads| {
        reads
            .borrow_mut()
            .as_mut()
            .and_then(|reads| cache(reads).get(&key).cloned())
    }) {
        return value;
    }
    let value = read();
    READS.with(|reads| {
        if let Some(reads) = reads.borrow_mut().as_mut() {
            cache(reads).insert(key, value.clone());
        }
    });
    value
}

pub(super) fn file(pid: Option<u32>, name: &'static str) -> Option<Arc<[u8]>> {
    cached(
        (pid, name),
        |reads| &mut reads.files,
        || {
            #[cfg(any(test, feature = "testkit"))]
            {
                use crate::testkit::host_pass::{Counter, count};
                let counter = match (pid, name) {
                    (None, "stat") => Counter::ProcBootStat,
                    (_, "stat") => Counter::ProcStat,
                    (_, "cmdline") => Counter::ProcCmdline,
                    (_, "comm") => Counter::ProcComm,
                    (_, "io") => Counter::ProcIo,
                    (_, "status") => Counter::ProcStatus,
                    _ => unreachable!("only the counted process files use this reader"),
                };
                count(counter);
            }
            let path = match pid {
                Some(pid) => format!("/proc/{pid}/{name}"),
                None => format!("/proc/{name}"),
            };
            std::fs::read(path).ok().map(Arc::from)
        },
    )
}

pub(super) fn text<T>(
    pid: Option<u32>,
    name: &'static str,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Option<T> {
    parse(std::str::from_utf8(&file(pid, name)?).ok()?)
}

pub(super) fn cwd(pid: u32, read: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    cached(pid, |reads| &mut reads.cwds, read)
}

pub(super) fn children(pid: u32, read: impl FnOnce() -> Vec<u32>) -> Vec<u32> {
    cached(pid, |reads| &mut reads.children, read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn changed_cmdline_is_visible_in_the_next_candidate_even_after_unwind() {
        let source = Cell::new("claude");
        let cmdline = || {
            cached(
                (Some(42), "cmdline"),
                |reads| &mut reads.files,
                || Some(Arc::<[u8]>::from(source.get().as_bytes())),
            )
        };
        scope(|| {
            assert_eq!(&*cmdline().unwrap(), b"claude");
            source.set("codex");
            assert_eq!(
                &*cmdline().unwrap(),
                b"claude",
                "one candidate shares its reads"
            );
        });
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scope(|| {
                cmdline();
                source.set("pi");
                panic!("discard the failed candidate");
            });
        }));
        scope(|| assert_eq!(&*cmdline().unwrap(), b"pi"));
        source.set("claude");
        assert_eq!(
            &*cmdline().unwrap(),
            b"claude",
            "nothing is cached outside production"
        );
    }
}
