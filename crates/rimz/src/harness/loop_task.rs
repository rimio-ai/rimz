//! The environment keys naming the loop task, and its booking chain, to the processes and
//! launches its fire starts. It belongs to the sidebar import graph; the account a fire's
//! launches use is `harness::task_login`.

pub(crate) const LOOP_TASK_ENV: &str = "RIMZ_LOOP_TASK";

/// The environment key carrying, as a JSON list, the booking chain the firing task held when
/// its run started (`RunFire::booked_by`), so an agent its check launches records it.
pub(crate) const LOOP_BOOKED_BY_ENV: &str = "RIMZ_LOOP_BOOKED_BY";
