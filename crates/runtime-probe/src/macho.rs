//! Minimal, bounded static Mach-O reader. Layouts/constants follow Apple's
//! mach-o/{fat,loader,nlist}.h; no platform headers are needed at build or runtime.
use crate::{Architecture, Export, Result, TARGET_EXPORTS};
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

const MAX_ARCHITECTURES: usize = 64;
const MAX_SYMBOLS: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 4096;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRIE_NODES: usize = 100_000;

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

fn range(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    offset
        .checked_add(size)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| "file offset or size is out of bounds".into())
}

impl Endian {
    fn u32(self, bytes: &[u8], offset: usize) -> Result<u32> {
        let value = range(bytes, offset, 4)?.try_into().unwrap();
        Ok(match self {
            Self::Little => u32::from_le_bytes(value),
            Self::Big => u32::from_be_bytes(value),
        })
    }

    fn u64(self, bytes: &[u8], offset: usize) -> Result<u64> {
        let value = range(bytes, offset, 8)?.try_into().unwrap();
        Ok(match self {
            Self::Little => u64::from_le_bytes(value),
            Self::Big => u64::from_be_bytes(value),
        })
    }

    fn usize(self, bytes: &[u8], offset: usize, wide: bool) -> Result<usize> {
        let value = if wide {
            self.u64(bytes, offset)?
        } else {
            self.u32(bytes, offset)? as u64
        };
        usize::try_from(value).map_err(|_| "file offset does not fit in memory".into())
    }
}

fn c_string(bytes: &[u8]) -> Result<&str> {
    let end = bytes
        .iter()
        .take(MAX_NAME_BYTES + 1)
        .position(|&byte| byte == 0)
        .ok_or("unterminated or oversized Mach-O string")?;
    std::str::from_utf8(&bytes[..end]).map_err(|_| "Mach-O string is not UTF-8".into())
}

pub(super) fn inspect(bytes: &[u8]) -> Result<Vec<Architecture>> {
    let (endian, wide) = match range(bytes, 0, 4)? {
        [0xca, 0xfe, 0xba, 0xbe] => (Endian::Big, false),
        [0xbe, 0xba, 0xfe, 0xca] => (Endian::Little, false),
        [0xca, 0xfe, 0xba, 0xbf] => (Endian::Big, true),
        [0xbf, 0xba, 0xfe, 0xca] => (Endian::Little, true),
        _ => return Ok(vec![inspect_thin(bytes)?.0]),
    };
    let count = endian.u32(bytes, 4)? as usize;
    if count == 0 || count > MAX_ARCHITECTURES {
        return Err("invalid or excessive fat architecture count".into());
    }
    let entry_size = if wide { 32 } else { 20 };
    let table_end = 8 + count * entry_size;
    range(bytes, 0, table_end)?;
    let mut regions = Vec::new();
    let mut architectures = Vec::new();
    for entry in bytes[8..table_end].chunks_exact(entry_size) {
        let cpu = endian.u32(entry, 0)?;
        let subtype = endian.u32(entry, 4)?;
        let offset = endian.usize(entry, 8, wide)?;
        let size = endian.usize(entry, if wide { 16 } else { 12 }, wide)?;
        let alignment = endian.u32(entry, if wide { 24 } else { 16 })?;
        if alignment > 31 || offset % (1usize << alignment) != 0 || offset < table_end {
            return Err("invalid fat architecture alignment or offset".into());
        }
        let slice = range(bytes, offset, size)?;
        let end = offset + size; // range already checked overflow.
        if regions
            .iter()
            .any(|&(start, stop)| offset < stop && start < end)
        {
            return Err("overlapping fat architecture slices".into());
        }
        regions.push((offset, end));
        let (architecture, identity) = inspect_thin(slice)?;
        if identity != (cpu, subtype) {
            return Err("fat and Mach-O architecture headers disagree".into());
        }
        architectures.push(architecture);
    }
    Ok(architectures)
}

fn architecture_name(cpu: u32, subtype: u32) -> String {
    match (cpu, subtype & 0x00ff_ffff) {
        (7, _) => "i386".into(),
        (0x0100_0007, 8) => "x86_64h".into(),
        (0x0100_0007, _) => "x86_64".into(),
        (12, 6) => "armv6".into(),
        (12, 9) => "armv7".into(),
        (12, 11) => "armv7s".into(),
        (12, _) => "arm".into(),
        (0x0100_000c, 2) => "arm64e".into(),
        (0x0100_000c, _) => "arm64".into(),
        (0x0200_000c, _) => "arm64_32".into(),
        (18, _) => "ppc".into(),
        (0x0100_0012, _) => "ppc64".into(),
        _ => format!("cpu-{cpu:#x}-subtype-{subtype:#x}"),
    }
}

fn inspect_thin(bytes: &[u8]) -> Result<(Architecture, (u32, u32))> {
    let (endian, wide) = match range(bytes, 0, 4)? {
        [0xce, 0xfa, 0xed, 0xfe] => (Endian::Little, false),
        [0xcf, 0xfa, 0xed, 0xfe] => (Endian::Little, true),
        [0xfe, 0xed, 0xfa, 0xce] => (Endian::Big, false),
        [0xfe, 0xed, 0xfa, 0xcf] => (Endian::Big, true),
        _ => return Err("unrecognized Mach-O magic".into()),
    };
    let header_size = if wide { 32 } else { 28 };
    range(bytes, 0, header_size)?;
    let cpu = endian.u32(bytes, 4)?;
    let subtype = endian.u32(bytes, 8)?;
    let command_count = endian.u32(bytes, 16)? as usize;
    let commands_size = endian.u32(bytes, 20)? as usize;
    let commands = range(bytes, header_size, commands_size)?;
    if command_count > 65_536 || command_count > commands.len() / 8 {
        return Err("invalid or excessive load command count".into());
    }
    let mut offset = 0;
    let mut install_name = None;
    let mut dependencies = Vec::new();
    let mut initializer_sections = BTreeSet::new();
    let mut symbol_table = None;
    let mut export_ranges = BTreeSet::new();
    for _ in 0..command_count {
        let command = endian.u32(commands, offset)?;
        let size = endian.u32(commands, offset + 4)? as usize;
        if size < 8 || size % (if wide { 8 } else { 4 }) != 0 {
            return Err("invalid load command size".into());
        }
        let data = range(commands, offset, size)?;
        match command {
            // LC_ID_DYLIB, LOAD, WEAK, REEXPORT, LAZY, UPWARD.
            0xd | 0xc | 0x8000_0018 | 0x8000_001f | 0x20 | 0x8000_0023 => {
                range(data, 0, 24)?;
                let name_offset = endian.u32(data, 8)? as usize;
                if name_offset < 24 {
                    return Err("dylib name overlaps its command header".into());
                }
                let name = c_string(range(
                    data,
                    name_offset,
                    data.len().saturating_sub(name_offset),
                )?)?;
                if name.is_empty() {
                    return Err("empty dylib install name".into());
                }
                if command == 0xd {
                    if install_name.replace(name.to_owned()).is_some() {
                        return Err("duplicate LC_ID_DYLIB".into());
                    }
                } else {
                    dependencies.push(name.to_owned());
                }
            }
            0x1 | 0x19 => {
                let segment_wide = command == 0x19;
                if segment_wide != wide {
                    return Err("segment and Mach-O header widths disagree".into());
                }
                let segment_size = if wide { 72 } else { 56 };
                let section_size = if wide { 80 } else { 68 };
                range(data, 0, segment_size)?;
                let count = endian.u32(data, if wide { 64 } else { 48 })? as usize;
                if count > (data.len() - segment_size) / section_size {
                    return Err("section count exceeds its segment command".into());
                }
                for section in data[segment_size..segment_size + count * section_size]
                    .chunks_exact(section_size)
                {
                    let name_bytes = &section[..16];
                    let end = name_bytes.iter().position(|&byte| byte == 0).unwrap_or(16);
                    let name = std::str::from_utf8(&name_bytes[..end])
                        .map_err(|_| "section name is not UTF-8")?;
                    let kind = endian.u32(section, if wide { 64 } else { 56 })? & 0xff;
                    if matches!(name, "__mod_init_func" | "__init_offsets")
                        || matches!(kind, 0x9 | 0x16)
                    {
                        initializer_sections.insert(name.to_owned());
                    }
                }
            }
            0x2 => {
                range(data, 0, 24)?;
                if symbol_table.replace(data).is_some() {
                    return Err("duplicate LC_SYMTAB".into());
                }
            }
            0x22 | 0x8000_0022 => {
                range(data, 0, 48)?;
                export_ranges.insert((
                    endian.u32(data, 40)? as usize,
                    endian.u32(data, 44)? as usize,
                ));
            }
            0x8000_0033 => {
                range(data, 0, 16)?;
                export_ranges.insert((
                    endian.u32(data, 8)? as usize,
                    endian.u32(data, 12)? as usize,
                ));
            }
            _ => {}
        }
        offset += size;
    }
    if offset != commands.len() {
        return Err("load command sizes do not match the Mach-O header".into());
    }
    let mut symbols = Symbols::default();
    if let Some(table) = symbol_table {
        read_symbols(bytes, table, endian, wide, &mut symbols)?;
    }
    for (offset, size) in export_ranges {
        if size != 0 {
            read_exports(range(bytes, offset, size)?, &mut symbols)?;
        }
    }
    let target_exports: Vec<_> = TARGET_EXPORTS
        .iter()
        .map(|&name| Export {
            name,
            present: symbols.names.contains(name),
        })
        .collect();
    Ok((
        Architecture {
            name: architecture_name(cpu, subtype),
            install_name,
            dependencies,
            initializer_section_present: !initializer_sections.is_empty(),
            initializer_sections: initializer_sections.into_iter().collect(),
            exported_symbol_count: symbols.names.len(),
            all_target_exports_present: target_exports.iter().all(|export| export.present),
            target_exports,
        },
        (cpu, subtype),
    ))
}

#[derive(Default)]
struct Symbols {
    names: BTreeSet<String>,
    text_bytes: usize,
}

impl Symbols {
    fn insert(&mut self, name: &str) -> Result<()> {
        // Preserve the former nm -gUj convention: remove exactly one C prefix.
        if let Some(name) = name.strip_prefix('_') {
            if name.len() > MAX_NAME_BYTES || self.text_bytes + name.len() > MAX_TEXT_BYTES {
                return Err("exported symbol names exceed inspection limits".into());
            }
            if !self.names.contains(name) {
                self.text_bytes += name.len();
                self.names.insert(name.to_owned());
            }
        }
        Ok(())
    }
}

fn read_symbols(
    bytes: &[u8],
    table: &[u8],
    endian: Endian,
    wide: bool,
    symbols: &mut Symbols,
) -> Result<()> {
    let count = endian.u32(table, 12)? as usize;
    if count > MAX_SYMBOLS {
        return Err("symbol count exceeds inspection limit".into());
    }
    let entry_size = if wide { 16 } else { 12 };
    let entries = range(bytes, endian.u32(table, 8)? as usize, count * entry_size)?;
    let strings = range(
        bytes,
        endian.u32(table, 16)? as usize,
        endian.u32(table, 20)? as usize,
    )?;
    for entry in entries.chunks_exact(entry_size) {
        let kind = entry[4];
        // External defined symbols only; omit STAB, private and undefined entries.
        if kind & 0xe0 != 0
            || kind & 0x10 != 0
            || kind & 1 == 0
            || !matches!(kind & 0x0e, 0x2 | 0xa | 0xe)
        {
            continue;
        }
        let index = endian.u32(entry, 0)? as usize;
        if index != 0 {
            symbols.insert(c_string(
                strings
                    .get(index..)
                    .ok_or("symbol string index is out of bounds")?,
            )?)?;
        }
    }
    Ok(())
}

fn uleb(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or("truncated export trie integer")?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err("overflowing export trie integer".into());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("overflowing export trie integer".into())
}

fn trie_offset(bytes: &[u8], cursor: &mut usize) -> Result<usize> {
    usize::try_from(uleb(bytes, cursor)?).map_err(|_| "export trie offset is too large".into())
}

fn read_exports(bytes: &[u8], symbols: &mut Symbols) -> Result<()> {
    // Explicit DFS avoids stack overflow and permits shared suffix nodes while
    // rejecting cycles along the current path. Bound visits as well as bytes.
    let mut pending = vec![(0, String::new(), false)];
    let mut active = BTreeSet::new();
    let mut visits = 0;
    let mut text_bytes = 0;
    while let Some((offset, prefix, leaving)) = pending.pop() {
        if leaving {
            active.remove(&offset);
            continue;
        }
        visits += 1;
        if !active.insert(offset) || visits > MAX_TRIE_NODES {
            return Err("cyclic or excessive export trie nodes".into());
        }
        pending.push((offset, String::new(), true));
        let mut cursor = offset;
        let terminal_size = trie_offset(bytes, &mut cursor)?;
        let terminal = range(bytes, cursor, terminal_size)?;
        cursor += terminal_size;
        if !terminal.is_empty() {
            if prefix.is_empty() {
                return Err("empty exported symbol in trie".into());
            }
            let mut position = 0;
            let flags = uleb(terminal, &mut position)?;
            if flags & !0x3f != 0 || flags & 3 == 3 {
                return Err("unsupported export trie flags".into());
            }
            uleb(terminal, &mut position)?; // Address, or dylib ordinal for reexports.
            if flags & 0x8 != 0 {
                let imported = c_string(&terminal[position..])?;
                position += imported.len() + 1;
            } else if flags & 0x10 != 0 {
                uleb(terminal, &mut position)?; // Resolver address; never called.
            }
            if position != terminal.len() {
                return Err("invalid export trie terminal size".into());
            }
            symbols.insert(&prefix)?;
        }
        let count = *bytes.get(cursor).ok_or("missing export trie child count")?;
        cursor += 1;
        for _ in 0..count {
            let suffix = c_string(
                bytes
                    .get(cursor..)
                    .ok_or("export trie edge is out of bounds")?,
            )?;
            if suffix.is_empty() || prefix.len() + suffix.len() > MAX_NAME_BYTES {
                return Err("empty or oversized export trie edge".into());
            }
            cursor += suffix.len() + 1;
            let child = trie_offset(bytes, &mut cursor)?;
            if child >= bytes.len() {
                return Err("export trie child is out of bounds".into());
            }
            text_bytes += prefix.len() + suffix.len();
            if text_bytes > MAX_TEXT_BYTES || pending.len() + visits >= MAX_TRIE_NODES {
                return Err("export trie exceeds inspection limits".into());
            }
            pending.push((child, format!("{prefix}{suffix}"), false));
        }
    }
    Ok(())
}
