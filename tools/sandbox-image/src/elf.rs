//! Whether a binary is static: an ELF file with no program interpreter (`PT_INTERP`), as a static or static-pie musl
//! binary is. The supervisor and the keeper must be: the sandbox image runs them on whatever libc it has.

use anyhow::{Result, bail};

const PT_INTERP: u32 = 3;

/// a 64-bit little-endian ELF file that names no interpreter
pub fn is_static(bytes: &[u8]) -> Result<bool> {
    if bytes.len() < 64 || &bytes[..4] != b"\x7fELF" {
        bail!("not an ELF file");
    }
    if bytes[4] != 2 || bytes[5] != 1 {
        bail!("not a 64-bit little-endian ELF file");
    }
    let u16_at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let phoff = usize::try_from(u64::from_le_bytes(bytes[0x20..0x28].try_into()?))?;
    let phentsize = usize::from(u16_at(0x36));
    let phnum = usize::from(u16_at(0x38));
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        let Some(t) = bytes.get(at..at + 4) else {
            bail!("program header {i} lies past the end of the file");
        };
        if u32::from_le_bytes(t.try_into()?) == PT_INTERP {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// an ELF header with the given program header types
    fn elf(types: &[u32]) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&u16::try_from(types.len()).unwrap().to_le_bytes());
        for t in types {
            let mut ph = vec![0u8; 56];
            ph[..4].copy_from_slice(&t.to_le_bytes());
            b.extend(ph);
        }
        b
    }

    #[test]
    fn a_binary_without_an_interpreter_is_static() {
        assert!(is_static(&elf(&[6, 1, 1, 2])).unwrap());
    }

    #[test]
    fn a_binary_with_an_interpreter_is_dynamic() {
        assert!(!is_static(&elf(&[6, PT_INTERP, 1])).unwrap());
    }

    #[test]
    fn what_is_not_elf_is_an_error() {
        assert!(is_static(b"#!/bin/sh\necho hi\n").is_err());
        let mut short = elf(&[1, 1]);
        short.truncate(80);
        assert!(is_static(&short).is_err());
    }

    #[test]
    fn this_test_binary_is_read() {
        // the test binary itself: a real ELF file of the build machine (dynamic on glibc, static on musl)
        let me = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        is_static(&me).unwrap();
    }
}
