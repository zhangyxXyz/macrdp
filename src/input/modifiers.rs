//! Remote modifier position policy, independent of Ctrl editing compatibility.

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ModifierLayout {
    #[default]
    Standard,
    SwapLeftWinAlt,
    SwapWinAlt,
}

impl ModifierLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::SwapLeftWinAlt => "swap-left-win-alt",
            Self::SwapWinAlt => "swap-win-alt",
        }
    }

    /// Apply once, before recording logical held state. The same immutable
    /// session policy maps down and up, preserving left/right identity.
    pub fn map(self, vk: u16) -> u16 {
        match (self, vk) {
            (Self::SwapLeftWinAlt | Self::SwapWinAlt, 0x37) => 0x3A,
            (Self::SwapLeftWinAlt | Self::SwapWinAlt, 0x3A) => 0x37,
            (Self::SwapWinAlt, 0x36) => 0x3D,
            (Self::SwapWinAlt, 0x3D) => 0x36,
            _ => vk,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
pub enum MaintenanceChord {
    Gather,
    Resync,
}

/// Match physical modifiers, so swapping positions never changes recovery keys.
#[cfg(any(target_os = "macos", test))]
pub fn maintenance_chord(
    vk: u16,
    ctrl: bool,
    alt: bool,
    win: bool,
    shift: bool,
) -> Option<MaintenanceChord> {
    match (vk, ctrl, alt, win, shift) {
        (0x05, true, true, false, _) => Some(MaintenanceChord::Gather),
        (0x0F, true, true, false, true) => Some(MaintenanceChord::Resync),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::scancodes::scancode_to_cgkeycode;

    #[test]
    fn physical_scancodes_map_to_the_intended_side_and_release() {
        for (scan, extended, standard, left, both) in [
            (0x5B, true, 0x37, 0x3A, 0x3A),
            (0x38, false, 0x3A, 0x37, 0x37),
            (0x5C, true, 0x36, 0x36, 0x3D),
            (0x38, true, 0x3D, 0x3D, 0x36),
            (0x1D, false, 0x3B, 0x3B, 0x3B),
            (0x1D, true, 0x3E, 0x3E, 0x3E),
        ] {
            let vk = scancode_to_cgkeycode(scan, extended).unwrap();
            for (policy, expected) in [
                (ModifierLayout::Standard, standard),
                (ModifierLayout::SwapLeftWinAlt, left),
                (ModifierLayout::SwapWinAlt, both),
            ] {
                let down = policy.map(vk);
                let up = policy.map(vk);
                assert_eq!((down, up), (expected, expected));
            }
        }
    }

    #[test]
    fn maintenance_requires_physical_alt_not_windows() {
        assert_eq!(
            maintenance_chord(0x05, true, true, false, false),
            Some(MaintenanceChord::Gather)
        );
        assert_eq!(
            maintenance_chord(0x0F, true, true, false, true),
            Some(MaintenanceChord::Resync)
        );
        assert_eq!(maintenance_chord(0x0F, true, true, false, false), None);
        assert_eq!(maintenance_chord(0x05, true, false, true, false), None);
        assert_eq!(maintenance_chord(0x05, true, true, true, false), None);
    }

    #[test]
    fn mapping_is_a_permutation_and_leaves_typing_and_other_modifiers_unchanged() {
        for policy in [
            ModifierLayout::Standard,
            ModifierLayout::SwapLeftWinAlt,
            ModifierLayout::SwapWinAlt,
        ] {
            for vk in 0..=255 {
                assert_eq!(policy.map(policy.map(vk)), vk);
                if !matches!(vk, 0x37 | 0x3A | 0x36 | 0x3D) {
                    assert_eq!(policy.map(vk), vk);
                }
            }
        }
    }
}
