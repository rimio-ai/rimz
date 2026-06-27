//! Built-in pet catalog and fixed sheet geometry shared by every source.

pub(crate) const FRAME_WIDTH: u32 = 192;
pub(crate) const FRAME_HEIGHT: u32 = 208;
pub(crate) const SHEET_COLS: u32 = 8;
pub(crate) const SHEET_ROWS: u32 = 9;
pub(crate) const SHEET_WIDTH: u32 = FRAME_WIDTH * SHEET_COLS;
pub(crate) const SHEET_HEIGHT: u32 = FRAME_HEIGHT * SHEET_ROWS;
pub(crate) const FRAME_COUNT: usize = (SHEET_COLS * SHEET_ROWS) as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pet {
    pub(crate) id: &'static str,
    pub(crate) file: &'static str,
}

pub(crate) const BUILTIN_PETS: &[Pet] = &[
    Pet {
        id: "codex",
        file: "codex-spritesheet-v4.webp",
    },
    Pet {
        id: "dewey",
        file: "dewey-spritesheet-v4.webp",
    },
    Pet {
        id: "fireball",
        file: "fireball-spritesheet-v4.webp",
    },
    Pet {
        id: "rocky",
        file: "rocky-spritesheet-v4.webp",
    },
    Pet {
        id: "seedy",
        file: "seedy-spritesheet-v4.webp",
    },
    Pet {
        id: "stacky",
        file: "stacky-spritesheet-v4.webp",
    },
    Pet {
        id: "bsod",
        file: "bsod-spritesheet-v4.webp",
    },
    Pet {
        id: "null-signal",
        file: "null-signal-spritesheet-v4.webp",
    },
];

pub(crate) fn pet_by_id(id: &str) -> Option<&'static Pet> {
    BUILTIN_PETS.iter().find(|pet| pet.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_contains_expected_ids() {
        let ids = BUILTIN_PETS.iter().map(|pet| pet.id).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "codex",
                "dewey",
                "fireball",
                "rocky",
                "seedy",
                "stacky",
                "bsod",
                "null-signal",
            ]
        );
    }
}
