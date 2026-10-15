use super::*;

fn put(bytes: &mut [u8], offset: usize, value: u32, endian: Endian) {
    bytes[offset..offset + 4].copy_from_slice(&match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn command(kind: u32, size: usize, endian: Endian) -> Vec<u8> {
    let mut bytes = vec![0; size];
    put(&mut bytes, 0, kind, endian);
    put(&mut bytes, 4, size as u32, endian);
    bytes
}

fn dylib(kind: u32, name: &str, endian: Endian, wide: bool) -> Vec<u8> {
    let align = if wide { 8 } else { 4 };
    let size = (24 + name.len() + 1).div_ceil(align) * align;
    let mut bytes = command(kind, size, endian);
    put(&mut bytes, 8, 24, endian);
    bytes[24..24 + name.len()].copy_from_slice(name.as_bytes());
    bytes
}

fn thin(endian: Endian, wide: bool, cpu: u32, commands: &[Vec<u8>], payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; if wide { 32 } else { 28 }];
    put(
        &mut bytes,
        0,
        if wide { 0xfeedfacf } else { 0xfeedface },
        endian,
    );
    put(&mut bytes, 4, cpu, endian);
    put(&mut bytes, 8, 0, endian);
    put(&mut bytes, 12, 6, endian); // MH_DYLIB.
    put(&mut bytes, 16, commands.len() as u32, endian);
    put(
        &mut bytes,
        20,
        commands.iter().map(Vec::len).sum::<usize>() as u32,
        endian,
    );
    for command in commands {
        bytes.extend(command);
    }
    bytes.extend(payload);
    bytes
}

fn fixture(endian: Endian, wide: bool, cpu: u32) -> Vec<u8> {
    let mut segment = command(
        if wide { 0x19 } else { 1 },
        if wide { 152 } else { 124 },
        endian,
    );
    put(&mut segment, if wide { 64 } else { 48 }, 1, endian);
    let section = if wide { 72 } else { 56 };
    segment[section..section + 14].copy_from_slice(b"__init_offsets");
    put(
        &mut segment,
        section + if wide { 64 } else { 56 },
        0x16,
        endian,
    );
    let mut commands = vec![
        dylib(0xd, "@rpath/steamclient.dylib", endian, wide),
        dylib(0xc, "@loader_path/a library.dylib", endian, wide),
        dylib(0x8000_0018, "/usr/lib/weak.dylib", endian, wide),
        dylib(0x8000_001f, "/usr/lib/reexport.dylib", endian, wide),
        segment,
    ];
    let names = [
        ("_CreateInterface", 0x0f),
        ("_Steam_BLoggedOn", 0x0f),
        ("_Steam_BConnected", 0x01),        // Undefined is not an export.
        ("_Steam_CreateSteamPipe", 0x0e),   // Local is not an export.
        ("_Steam_ReleaseUser", 0x1f),       // Private external is not an export.
        ("_Steam_FreeLastCallback", 0xef),  // STAB is not an export.
        ("__Steam_CreateGlobalUser", 0x0f), // Remove exactly one underscore.
        ("Steam_CreateLocalUser", 0x0f),    // No Mach-O C prefix.
        ("_CreateInterface", 0x0f),         // Count unique names.
    ];
    let mut strings = vec![0];
    let entry_size = if wide { 16 } else { 12 };
    let mut entries = vec![0; names.len() * entry_size];
    for (entry, (name, kind)) in entries.chunks_exact_mut(entry_size).zip(names) {
        put(entry, 0, strings.len() as u32, endian);
        entry[4] = kind;
        strings.extend(name.bytes());
        strings.push(0);
    }
    let mut table = command(2, 24, endian);
    let offset =
        if wide { 32 } else { 28 } + commands.iter().map(Vec::len).sum::<usize>() + table.len();
    put(&mut table, 8, offset as u32, endian);
    put(&mut table, 12, names.len() as u32, endian);
    put(&mut table, 16, (offset + entries.len()) as u32, endian);
    put(&mut table, 20, strings.len() as u32, endian);
    commands.push(table);
    entries.extend(strings);
    thin(endian, wide, cpu, &commands, &entries)
}

#[test]
fn reads_thin_endianness_widths_and_real_symbol_kinds() {
    for endian in [Endian::Little, Endian::Big] {
        for (wide, cpu, name) in [(false, 7, "i386"), (true, 0x0100_0007, "x86_64")] {
            let report = inspect(&fixture(endian, wide, cpu)).unwrap().remove(0);
            assert_eq!(report.name, name);
            assert_eq!(
                report.install_name.as_deref(),
                Some("@rpath/steamclient.dylib")
            );
            assert_eq!(
                report.dependencies,
                [
                    "@loader_path/a library.dylib",
                    "/usr/lib/weak.dylib",
                    "/usr/lib/reexport.dylib"
                ]
            );
            assert_eq!(report.initializer_sections, ["__init_offsets"]);
            assert_eq!(report.exported_symbol_count, 3);
            assert_eq!(
                report
                    .target_exports
                    .iter()
                    .filter(|e| e.present)
                    .map(|e| e.name)
                    .collect::<Vec<_>>(),
                ["CreateInterface", "Steam_BLoggedOn"]
            );
            assert!(!report.all_target_exports_present);
        }
    }
}

fn fat(wide: bool, endian: Endian) -> Vec<u8> {
    let slices = [
        (0x0100_0007, fixture(Endian::Little, true, 0x0100_0007)),
        (0x0100_000c, fixture(Endian::Little, true, 0x0100_000c)),
    ];
    let entry_size = if wide { 32 } else { 20 };
    let mut bytes = vec![0; 8 + slices.len() * entry_size];
    put(
        &mut bytes,
        0,
        if wide { 0xcafebabf } else { 0xcafebabe },
        endian,
    );
    put(&mut bytes, 4, slices.len() as u32, endian);
    for (index, (cpu, slice)) in slices.into_iter().enumerate() {
        let entry = 8 + index * entry_size;
        let offset = bytes.len();
        put(&mut bytes, entry, cpu, endian);
        let low = if wide && matches!(endian, Endian::Big) {
            4
        } else {
            0
        };
        put(&mut bytes, entry + 8 + low, offset as u32, endian);
        put(
            &mut bytes,
            entry + (if wide { 16 } else { 12 }) + low,
            slice.len() as u32,
            endian,
        );
        bytes.extend(slice);
    }
    bytes
}

#[test]
fn reads_fat32_and_fat64_in_both_byte_orders() {
    for endian in [Endian::Little, Endian::Big] {
        for wide in [false, true] {
            let report = inspect(&fat(wide, endian)).unwrap();
            assert_eq!(
                report.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
                ["x86_64", "arm64"]
            );
        }
    }
}

fn trie_fixture(trie: &[u8], modern: bool) -> Vec<u8> {
    let mut export = command(
        if modern { 0x8000_0033 } else { 0x8000_0022 },
        if modern { 16 } else { 48 },
        Endian::Little,
    );
    let offset = 32 + export.len();
    put(
        &mut export,
        if modern { 8 } else { 40 },
        offset as u32,
        Endian::Little,
    );
    put(
        &mut export,
        if modern { 12 } else { 44 },
        trie.len() as u32,
        Endian::Little,
    );
    thin(Endian::Little, true, 0x0100_000c, &[export], trie)
}

fn export_trie(terminal: &[u8]) -> Vec<u8> {
    let mut trie = vec![0, 1]; // Root, one edge.
    trie.extend(b"_CreateInterface\0");
    trie.push((trie.len() + 1) as u8);
    trie.push(terminal.len() as u8);
    trie.extend(terminal);
    trie.push(0); // Leaf, no children.
    trie
}

#[test]
fn reads_stripped_exports_reexports_and_resolvers_without_executing_them() {
    for modern in [false, true] {
        for terminal in [&[0, 42][..], &[8, 1, 0], &[0x10, 1, 2]] {
            let report = inspect(&trie_fixture(&export_trie(terminal), modern))
                .unwrap()
                .remove(0);
            assert_eq!(report.exported_symbol_count, 1);
            assert!(report.target_exports[0].present);
        }
    }
}

#[test]
fn reads_shared_export_leaves_without_confusing_them_with_cycles() {
    // Two exported names can share the same terminal (for example, aliases).
    let mut trie = vec![0, 2, b'_', b'a', 0, 10, b'_', b'b', 0, 10];
    trie.extend([2, 0, 42, 0]);
    let report = inspect(&trie_fixture(&trie, true)).unwrap().remove(0);
    assert_eq!(report.exported_symbol_count, 2);
}

#[test]
fn rejects_every_truncation_of_a_complete_fat_file() {
    let bytes = fat(true, Endian::Big);
    for length in 0..bytes.len() {
        assert!(
            inspect(&bytes[..length]).is_err(),
            "accepted truncation at {length}"
        );
    }
}

#[test]
fn rejects_malformed_load_commands_sections_and_symbol_offsets() {
    let valid = fixture(Endian::Little, true, 0x0100_0007);
    for (offset, value) in [
        (16, u32::MAX),
        (20, u32::MAX),
        (36, 0),
        (36, u32::MAX),
        (40, 1),
        (40, u32::MAX),
    ] {
        let mut broken = valid.clone();
        put(&mut broken, offset, value, Endian::Little);
        assert!(
            inspect(&broken).is_err(),
            "accepted corrupt offset {offset}"
        );
    }
    let mut offset = 32;
    while offset < 32 + Endian::Little.u32(&valid, 20).unwrap() as usize {
        let kind = Endian::Little.u32(&valid, offset).unwrap();
        for field in match kind {
            0x19 => vec![64],
            2 => vec![8, 12, 16, 20],
            _ => vec![],
        } {
            let mut broken = valid.clone();
            put(&mut broken, offset + field, u32::MAX, Endian::Little);
            assert!(inspect(&broken).is_err());
        }
        offset += Endian::Little.u32(&valid, offset + 4).unwrap() as usize;
    }
}

#[test]
fn rejects_fat_overflow_overlap_and_header_mismatch() {
    for (offset, value) in [
        (4, u32::MAX),
        (8, 7),
        (16, u32::MAX),
        (20, u32::MAX),
        (24, u32::MAX),
        (32, 32),
        (48, u32::MAX),
        (52, 72),
    ] {
        let mut broken = fat(true, Endian::Big);
        put(&mut broken, offset, value, Endian::Big);
        assert!(
            inspect(&broken).is_err(),
            "accepted corrupt fat field {offset}"
        );
    }
}

#[test]
fn rejects_cyclic_overflowing_and_truncated_export_tries() {
    for trie in [
        vec![0, 1, b'_', 0, 0],
        vec![0xff; 11],
        vec![0, 1, b'_', 0, 0x7f],
        export_trie(&[0xff; 10]),
        export_trie(&[8, 1]),
        export_trie(&[0x10, 1]),
    ] {
        assert!(inspect(&trie_fixture(&trie, true)).is_err());
    }
    let bytes = trie_fixture(&export_trie(&[0, 42]), true);
    for length in 0..bytes.len() {
        assert!(inspect(&bytes[..length]).is_err());
    }
}

#[test]
fn rejects_long_trie_names_and_unterminated_symbol_names() {
    let mut trie = vec![0, 1];
    trie.extend(vec![b'x'; MAX_NAME_BYTES + 1]);
    trie.extend([0, 0]);
    assert!(inspect(&trie_fixture(&trie, true)).is_err());
    let mut bytes = fixture(Endian::Little, true, 0x0100_0007);
    *bytes.last_mut().unwrap() = b'x';
    assert!(inspect(&bytes).is_err());
}

#[test]
fn arbitrary_short_inputs_never_panic() {
    let mut seed = 0x1234_5678u32;
    for length in 0..512 {
        let bytes: Vec<_> = (0..length)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        assert!(inspect(&bytes).is_err());
    }
}
