//! Classify machine PSI into the footer's active and fading notices.

use crate::proc::pressure::{Pressure, Resource};
use crate::store::snapshot::{SidebarStallItem, SidebarStallPhase, SidebarStallResource};

/// Read the room host's machine/container pressure once for a publication cycle.
pub fn read() -> Vec<SidebarStallItem> {
    classify(
        [Resource::Memory, Resource::Cpu, Resource::Io]
            .into_iter()
            .filter_map(|resource| {
                crate::proc::pressure::read(resource)
                    .ok()
                    .map(|pressure| (resource, pressure))
            }),
    )
}

fn classify(readings: impl IntoIterator<Item = (Resource, Pressure)>) -> Vec<SidebarStallItem> {
    let mut items = Vec::new();
    for (resource, pressure) in readings {
        let (resource, threshold) = match resource {
            Resource::Memory => (SidebarStallResource::Memory, 10.0),
            Resource::Cpu => (SidebarStallResource::Cpu, 50.0),
            Resource::Io => (SidebarStallResource::Io, 25.0),
        };
        if !(pressure.avg10 >= threshold || pressure.avg60 >= threshold) {
            continue;
        }
        items.push(SidebarStallItem {
            resource,
            percent: pressure.avg10.round().clamp(0.0, 100.0) as u8,
            phase: if pressure.avg10 >= 0.8 * threshold {
                SidebarStallPhase::Active
            } else {
                SidebarStallPhase::Fading
            },
        });
    }
    items.sort_unstable_by_key(|item| (item.phase, item.resource));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use SidebarStallPhase::{Active, Fading};

    #[test]
    fn thresholds_active_band_rounding_and_clamping_cover_each_resource() {
        let cases = [
            (Resource::Memory, 9.99, 9.99, None),
            (Resource::Memory, 10.0, 0.0, Some((10, Active))),
            (Resource::Memory, 7.99, 10.0, Some((8, Fading))),
            (Resource::Memory, 8.0, 10.0, Some((8, Active))),
            (Resource::Cpu, 49.99, 49.99, None),
            (Resource::Cpu, 50.0, 0.0, Some((50, Active))),
            (Resource::Cpu, 39.99, 50.0, Some((40, Fading))),
            (Resource::Cpu, 40.0, 50.0, Some((40, Active))),
            (Resource::Io, 24.99, 24.99, None),
            (Resource::Io, 25.0, 0.0, Some((25, Active))),
            (Resource::Io, 19.99, 25.0, Some((20, Fading))),
            (Resource::Io, 20.0, 25.0, Some((20, Active))),
            (Resource::Memory, 9.5, 10.0, Some((10, Active))),
            (Resource::Memory, 0.4, 10.0, Some((0, Fading))),
            (Resource::Memory, -1.0, 10.0, Some((0, Fading))),
            (Resource::Cpu, 101.7, 0.0, Some((100, Active))),
        ];
        let actual = cases.map(|(resource, avg10, avg60, _)| {
            classify([(resource, Pressure { avg10, avg60 })])
                .first()
                .map(|item| (item.percent, item.phase))
        });
        assert_eq!(actual, cases.map(|(_, _, _, expected)| expected));
    }

    #[test]
    fn phases_then_resources_define_order_and_wire_shape() {
        let actual = [
            classify([
                (
                    Resource::Memory,
                    Pressure {
                        avg10: 6.0,
                        avg60: 12.0,
                    },
                ),
                (
                    Resource::Io,
                    Pressure {
                        avg10: 51.0,
                        avg60: 0.0,
                    },
                ),
                (
                    Resource::Cpu,
                    Pressure {
                        avg10: 81.0,
                        avg60: 0.0,
                    },
                ),
            ]),
            classify([
                (
                    Resource::Io,
                    Pressure {
                        avg10: 51.0,
                        avg60: 0.0,
                    },
                ),
                (
                    Resource::Cpu,
                    Pressure {
                        avg10: 88.0,
                        avg60: 0.0,
                    },
                ),
                (
                    Resource::Memory,
                    Pressure {
                        avg10: 22.0,
                        avg60: 0.0,
                    },
                ),
            ]),
            classify([]),
        ];
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::json!([
                [
                    {"resource":"cpu", "percent":81, "phase":"active"},
                    {"resource":"io", "percent":51, "phase":"active"},
                    {"resource":"memory", "percent":6, "phase":"fading"}
                ],
                [
                    {"resource":"memory", "percent":22, "phase":"active"},
                    {"resource":"cpu", "percent":88, "phase":"active"},
                    {"resource":"io", "percent":51, "phase":"active"}
                ],
                []
            ])
        );
    }
}
