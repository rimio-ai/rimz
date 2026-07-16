//! Build identity of the running executable.
//!
//! Durable sidebar artifacts are written by whichever process holds the role
//! at the time, and across an upgrade old and new builds overlap inside one
//! workspace. Stamping each published pane frame and diagnostic record with
//! the writer's build id turns that overlap into recorded evidence. RimZ uses
//! the linker build identity (`.note.gnu.build-id` on ELF, `LC_UUID` on
//! Mach-O) and falls back to hashing the image bytes when the identity is not
//! available.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// Human-facing build version surfaced by CLI reports and sidebar heartbeats.
pub const VERSION: &str = match option_env!("RIMZ_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Bytes of build identity RimZ stamps into runtime artifacts.
const BUILD_ID_BYTES: usize = 6;
/// Bytes of the image prefix scanned for the linker build id.
const IMAGE_PREFIX_LEN: usize = 1 << 20;
static BUILD_ID: OnceLock<Option<String>> = OnceLock::new();

/// Build id of this process, computed once from the executable's linker
/// identity; `None` when the binary cannot be read (for example replaced
/// mid-upgrade before the re-exec lands).
pub fn current() -> Option<&'static str> {
    BUILD_ID.get_or_init(compute).as_deref()
}

/// Return this process's build id only if a prior [`warm`] or [`current`] call
/// has already computed it.
pub fn current_if_ready() -> Option<&'static str> {
    BUILD_ID.get().and_then(Option::as_deref)
}

/// Start computing this process's build id on a background thread.
pub fn warm() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| {
            let _ = current();
        });
    });
}

fn compute() -> Option<String> {
    of_file(&running_image_path()?).ok()
}

/// Read the linker build identity at `path` into the short build id RimZ
/// stamps into runtime artifacts. Hash the image bytes when the identity is not
/// available.
pub fn of_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    if let Some(id) = linked_build_id(&mut file)? {
        return Ok(id);
    }
    file.seek(SeekFrom::Start(0))?;
    hash_file(&mut file)
}

/// Read a build identity from an already-captured executable image. Staging
/// uses this so the identity and copied bytes come from one source snapshot.
pub(crate) fn of_bytes(image: &[u8]) -> String {
    build_id_from_image(image)
        .filter(|desc| desc.len() >= BUILD_ID_BYTES)
        .map(|desc| hex::encode(&desc[..BUILD_ID_BYTES]))
        .unwrap_or_else(|| {
            let digest = Sha256::digest(image);
            hex::encode(&digest[..BUILD_ID_BYTES])
        })
}

fn linked_build_id(file: &mut std::fs::File) -> io::Result<Option<String>> {
    let mut buf = vec![0_u8; IMAGE_PREFIX_LEN];
    let n = read_up_to(file, &mut buf)?;
    buf.truncate(n);
    Ok(build_id_from_image(&buf)
        .filter(|desc| desc.len() >= BUILD_ID_BYTES)
        .map(|desc| hex::encode(&desc[..BUILD_ID_BYTES])))
}

fn hash_file(file: &mut std::fs::File) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(hex::encode(&digest[..BUILD_ID_BYTES]))
}

fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let read = file.read(&mut buf[n..])?;
        if read == 0 {
            break;
        }
        n += read;
    }
    Ok(n)
}

fn build_id_from_image(image: &[u8]) -> Option<Vec<u8>> {
    match image.get(..4)? {
        [0x7F, b'E', b'L', b'F'] => build_id_from_elf(image),
        [0xFE, 0xED, 0xFA, 0xCE] => build_id_from_macho(image, Endian::Big, false),
        [0xCE, 0xFA, 0xED, 0xFE] => build_id_from_macho(image, Endian::Little, false),
        [0xFE, 0xED, 0xFA, 0xCF] => build_id_from_macho(image, Endian::Big, true),
        [0xCF, 0xFA, 0xED, 0xFE] => build_id_from_macho(image, Endian::Little, true),
        [0xCA, 0xFE, 0xBA, 0xBE] | [0xBE, 0xBA, 0xFE, 0xCA] => None,
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

fn build_id_from_elf(image: &[u8]) -> Option<Vec<u8>> {
    let class = *image.get(4)?;
    let endian = match *image.get(5)? {
        1 => Endian::Little,
        2 => Endian::Big,
        _ => return None,
    };

    let (phoff, phentsize, phnum, p_offset_offset, p_filesz_offset, required_phdr_len) = match class
    {
        1 => (
            usize::try_from(read_u32_at(image, 0x1C, endian)?).ok()?,
            usize::from(read_u16_at(image, 0x2A, endian)?),
            usize::from(read_u16_at(image, 0x2C, endian)?),
            4,
            0x10,
            0x14,
        ),
        2 => (
            usize::try_from(read_u64_at(image, 0x20, endian)?).ok()?,
            usize::from(read_u16_at(image, 0x36, endian)?),
            usize::from(read_u16_at(image, 0x38, endian)?),
            8,
            0x20,
            0x28,
        ),
        _ => return None,
    };
    if phentsize < required_phdr_len {
        return None;
    }

    for i in 0..phnum {
        let phdr_offset = phoff.checked_add(i.checked_mul(phentsize)?)?;
        let phdr = image.get(phdr_offset..phdr_offset.checked_add(required_phdr_len)?)?;
        if read_u32_at(phdr, 0, endian)? != 4 {
            continue;
        }
        let (note_offset, note_len) = match class {
            1 => (
                usize::try_from(read_u32_at(phdr, p_offset_offset, endian)?).ok()?,
                usize::try_from(read_u32_at(phdr, p_filesz_offset, endian)?).ok()?,
            ),
            2 => (
                usize::try_from(read_u64_at(phdr, p_offset_offset, endian)?).ok()?,
                usize::try_from(read_u64_at(phdr, p_filesz_offset, endian)?).ok()?,
            ),
            _ => return None,
        };
        let notes = image.get(note_offset..note_offset.checked_add(note_len)?)?;
        if let Some(desc) = find_gnu_build_id(notes, endian) {
            return Some(desc);
        }
    }
    None
}

fn find_gnu_build_id(notes: &[u8], endian: Endian) -> Option<Vec<u8>> {
    let mut offset = 0;
    while offset < notes.len() {
        let namesz = usize::try_from(read_u32_at(notes, offset, endian)?).ok()?;
        let descsz = usize::try_from(read_u32_at(notes, offset.checked_add(4)?, endian)?).ok()?;
        let note_type = read_u32_at(notes, offset.checked_add(8)?, endian)?;
        offset = offset.checked_add(12)?;

        let name_end = offset.checked_add(namesz)?;
        let name = notes.get(offset..name_end)?;
        offset = align4(name_end)?;

        let desc_end = offset.checked_add(descsz)?;
        let desc = notes.get(offset..desc_end)?;
        offset = align4(desc_end)?;
        if offset > notes.len() {
            return None;
        }

        if note_type == 3 && name == b"GNU\0" {
            return Some(desc.to_vec());
        }
    }
    None
}

fn build_id_from_macho(image: &[u8], endian: Endian, is_64_bit: bool) -> Option<Vec<u8>> {
    let ncmds = usize::try_from(read_u32_at(image, 0x10, endian)?).ok()?;
    let mut offset = if is_64_bit { 32 } else { 28 };

    for _ in 0..ncmds {
        let cmd = read_u32_at(image, offset, endian)?;
        let cmdsize = usize::try_from(read_u32_at(image, offset.checked_add(4)?, endian)?).ok()?;
        if cmdsize < 8 {
            return None;
        }
        let command_end = offset.checked_add(cmdsize)?;
        image.get(offset..command_end)?;
        if cmd == 0x1B {
            let uuid_start = offset.checked_add(8)?;
            let uuid_end = uuid_start.checked_add(16)?;
            if uuid_end > command_end {
                return None;
            }
            return Some(image.get(uuid_start..uuid_end)?.to_vec());
        }
        offset = command_end;
    }
    None
}

fn align4(n: usize) -> Option<usize> {
    n.checked_add(3).map(|n| n & !3)
}

fn read_u16_at(bytes: &[u8], offset: usize, endian: Endian) -> Option<u16> {
    let bytes = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u16::from_le_bytes(bytes),
        Endian::Big => u16::from_be_bytes(bytes),
    })
}

fn read_u32_at(bytes: &[u8], offset: usize, endian: Endian) -> Option<u32> {
    let bytes = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u32::from_le_bytes(bytes),
        Endian::Big => u32::from_be_bytes(bytes),
    })
}

fn read_u64_at(bytes: &[u8], offset: usize, endian: Endian) -> Option<u64> {
    let bytes = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u64::from_le_bytes(bytes),
        Endian::Big => u64::from_be_bytes(bytes),
    })
}

/// Resolve an executable path reported by the OS to the replacement binary on
/// disk. Linux annotates the running image path with " (deleted)" after an
/// atomic install unlinks that inode; the replacement lives at the stripped
/// path.
pub fn resolve_on_disk_binary(exe: &Path) -> Option<PathBuf> {
    crate::proc::resolve_existing_or_replacement(exe)
}

#[cfg(target_os = "linux")]
fn running_image_path() -> Option<PathBuf> {
    Some(PathBuf::from("/proc/self/exe"))
}

#[cfg(not(target_os = "linux"))]
fn running_image_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_starts_with_package_version() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")), "{VERSION}");
    }

    fn write_u16_le(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn elf64_le_with_note(desc: &[u8]) -> Vec<u8> {
        let note_offset = 0x100;
        let mut note = Vec::new();
        note.extend_from_slice(&4_u32.to_le_bytes());
        note.extend_from_slice(&(desc.len() as u32).to_le_bytes());
        note.extend_from_slice(&3_u32.to_le_bytes());
        note.extend_from_slice(b"GNU\0");
        note.extend_from_slice(desc);
        while note.len() % 4 != 0 {
            note.push(0);
        }

        let mut image = vec![0_u8; note_offset + note.len()];
        image[..4].copy_from_slice(b"\x7FELF");
        image[4] = 2;
        image[5] = 1;
        write_u64_le(&mut image, 0x20, 64);
        write_u16_le(&mut image, 0x36, 56);
        write_u16_le(&mut image, 0x38, 1);

        write_u32_le(&mut image, 64, 4);
        write_u64_le(&mut image, 64 + 8, note_offset as u64);
        write_u64_le(&mut image, 64 + 0x20, note.len() as u64);
        image[note_offset..].copy_from_slice(&note);
        image
    }

    #[test]
    fn build_id_is_stable_lowercase_hex() {
        let first = current().expect("the test binary is readable");
        let second = current().expect("the second call serves the cached id");

        assert_eq!(first, second);
        assert_eq!(first.len(), BUILD_ID_BYTES * 2);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn parse_elf64_le_build_id_note() {
        let desc: Vec<u8> = (0..20).collect();
        let image = elf64_le_with_note(&desc);

        assert_eq!(build_id_from_image(&image), Some(desc.clone()));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rimz");
        std::fs::write(&path, &image).unwrap();
        let mut file = std::fs::File::open(path).unwrap();

        assert_eq!(
            linked_build_id(&mut file).unwrap(),
            Some(hex::encode(&desc[..BUILD_ID_BYTES]))
        );
    }

    #[test]
    fn parse_macho64_le_uuid() {
        let uuid: Vec<u8> = (0xA0..0xB0).collect();
        let mut image = vec![0_u8; 32 + 16 + 24];
        image[..4].copy_from_slice(b"\xCF\xFA\xED\xFE");
        write_u32_le(&mut image, 0x10, 2);
        write_u32_le(&mut image, 0x20, 0x19);
        write_u32_le(&mut image, 0x24, 16);
        write_u32_le(&mut image, 0x30, 0x1B);
        write_u32_le(&mut image, 0x34, 24);
        image[0x38..0x48].copy_from_slice(&uuid);

        assert_eq!(build_id_from_image(&image), Some(uuid));
    }

    #[test]
    fn no_note_falls_back_to_hash() {
        let bytes = b"plain bytes without a linker note";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rimz");
        std::fs::write(&path, bytes).unwrap();

        let mut file = std::fs::File::open(&path).unwrap();
        assert_eq!(linked_build_id(&mut file).unwrap(), None);

        let digest = Sha256::digest(bytes);
        assert_eq!(
            of_file(&path).unwrap(),
            hex::encode(&digest[..BUILD_ID_BYTES])
        );
    }

    #[test]
    fn garbage_and_truncated_images_return_none() {
        assert_eq!(build_id_from_image(&[]), None);
        assert_eq!(build_id_from_image(b"\x7FEL"), None);
        assert_eq!(build_id_from_image(b"nope"), None);

        let mut image = vec![0_u8; 120];
        image[..4].copy_from_slice(b"\x7FELF");
        image[4] = 2;
        image[5] = 1;
        write_u64_le(&mut image, 0x20, 64);
        write_u16_le(&mut image, 0x36, 56);
        write_u16_le(&mut image, 0x38, 1);
        write_u32_le(&mut image, 64, 4);
        write_u64_le(&mut image, 64 + 8, 200);
        write_u64_le(&mut image, 64 + 0x20, 100);

        assert_eq!(build_id_from_image(&image), None);
    }

    #[test]
    fn of_file_on_current_exe_is_twelve_lowercase_hex() {
        let path = std::env::current_exe().unwrap();
        let id = of_file(&path).unwrap();

        assert_eq!(id.len(), BUILD_ID_BYTES * 2);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(Some(id.as_str()), current());
    }

    #[test]
    fn resolve_on_disk_binary_strips_deleted_suffix_when_replacement_exists() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("rimz");
        std::fs::write(&real, b"x").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));

        assert_eq!(resolve_on_disk_binary(&deleted), Some(real.clone()));
        assert_eq!(resolve_on_disk_binary(&real), Some(real));
    }

    #[test]
    fn resolve_on_disk_binary_returns_none_when_no_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("rimz");
        let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));

        assert_eq!(resolve_on_disk_binary(&deleted), None);
        assert_eq!(resolve_on_disk_binary(&missing), None);
    }
}
