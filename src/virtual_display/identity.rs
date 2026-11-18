//! Resolve WindowServer renumbering using the descriptor belonging to this
//! process. An external monitor can have exactly the same resolution.
#[derive(Clone, Copy)]
pub(super) struct Candidate {
    id: u32,
    vendor: u32,
    serial: u32,
    builtin: bool,
}

pub(super) const VENDOR: u32 = u32::from_be_bytes(*b"macr");

fn choose(live: u32, serial: u32, displays: &[Candidate]) -> Option<u32> {
    let mut owned = displays
        .iter()
        .filter(|d| !d.builtin && d.id != 0 && d.vendor == VENDOR && d.serial == serial);
    match (owned.next(), owned.next()) {
        (Some(d), None) => Some(d.id),
        (Some(_), Some(_)) => None,
        // Some OS versions report a virtual panel as 'unkn'. In that case
        // require confirmation from the owned CGVirtualDisplay object itself.
        _ => displays
            .iter()
            .find(|d| {
                d.id != 0
                    && d.id == live
                    && !d.builtin
                    && (d.vendor == 0 || d.vendor == u32::from_be_bytes(*b"unkn"))
            })
            .map(|d| d.id),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn resolve(live: u32, ids: &[u32]) -> Option<u32> {
    use core_graphics::display::CGDisplay;
    extern "C" {
        fn CGDisplayVendorNumber(id: u32) -> u32;
        fn CGDisplaySerialNumber(id: u32) -> u32;
    }
    let candidates: Vec<_> = ids
        .iter()
        .map(|&id| Candidate {
            id,
            vendor: unsafe { CGDisplayVendorNumber(id) },
            serial: unsafe { CGDisplaySerialNumber(id) },
            builtin: CGDisplay::new(id).is_builtin(),
        })
        .collect();
    choose(live, std::process::id(), &candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn panel(id: u32, vendor: u32, serial: u32) -> Candidate {
        Candidate {
            id,
            vendor,
            serial,
            builtin: false,
        }
    }
    #[test]
    fn follows_only_our_virtual_display_across_renumbering() {
        let ours = panel(23, VENDOR, 42);
        let dell = panel(22, 0x10ac, 100);
        let foreign = panel(24, VENDOR, 43);
        assert_eq!(choose(22, 42, &[dell, ours, foreign]), Some(23));
        assert_eq!(choose(23, 42, &[ours]), Some(23));
        assert_eq!(choose(22, 42, &[dell, foreign]), None);
        assert_eq!(choose(24, 42, &[foreign]), None);
        assert_eq!(choose(0, 42, &[]), None);
        assert_eq!(choose(23, 42, &[ours, panel(25, VENDOR, 42)]), None);
    }
    #[test]
    fn unknown_descriptor_requires_the_live_object_to_report_that_id() {
        let unknown = panel(23, u32::from_be_bytes(*b"unkn"), 0);
        assert_eq!(choose(23, 42, &[unknown]), Some(23));
        assert_eq!(choose(22, 42, &[unknown]), None);
        assert_eq!(
            choose(
                23,
                42,
                &[Candidate {
                    builtin: true,
                    ..unknown
                }]
            ),
            None
        );
    }
}
