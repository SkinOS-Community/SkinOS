use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Architecture {
    X86,
    X64,
}
impl Architecture {
    pub(crate) fn probe_name(self) -> &'static str {
        match self {
            Self::X86 => "sdk-probe32.exe",
            Self::X64 => "sdk-probe.exe",
        }
    }
    fn sdk_name(self) -> &'static str {
        match self {
            Self::X86 => "steam_api.dll",
            Self::X64 => "steam_api64.dll",
        }
    }
    pub(crate) fn client_name(self) -> &'static str {
        match self {
            Self::X86 => "steamclient.dll",
            Self::X64 => "steamclient64.dll",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X64 => "x64",
        }
    }
}

/// Read only the DOS/PE headers; the size of the installed game is irrelevant.
pub(crate) fn architecture(path: &Path) -> Result<Architecture, String> {
    let mut file =
        File::open(path).map_err(|e| format!("Не удалось прочитать {}: {e}", path.display()))?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos)
        .map_err(|_| "Повреждён DOS-заголовок Windows-файла")?;
    if &dos[..2] != b"MZ" {
        return Err("Исполняемый файл не имеет Windows PE-заголовка".into());
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;
    if !(64..=16 * 1024 * 1024).contains(&offset) {
        return Err("Некорректный адрес PE-заголовка".into());
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut pe = [0u8; 26];
    file.read_exact(&mut pe)
        .map_err(|_| "Неполный PE-заголовок Windows-файла")?;
    if &pe[..4] != b"PE\0\0" || u16::from_le_bytes([pe[20], pe[21]]) < 2 {
        return Err("Повреждён PE-заголовок Windows-файла".into());
    }
    match (
        u16::from_le_bytes([pe[4], pe[5]]),
        u16::from_le_bytes([pe[24], pe[25]]),
    ) {
        (0x14c, 0x10b) => Ok(Architecture::X86),
        (0x8664, 0x20b) => Ok(Architecture::X64),
        (machine, _) => Err(format!(
            "Windows-архитектура 0x{machine:04x} пока не поддерживается"
        )),
    }
}

pub(crate) struct Selection {
    pub architecture: Architecture,
    pub sdk: Option<PathBuf>,
}

/// The probe must match the selected executable, not the architecture of macOS.
/// Confine SDK candidates to this installation; never borrow another game's DLL.
pub(crate) fn select(install: &Path, executable: &Path) -> Result<Selection, String> {
    let install = install.canonicalize().map_err(|e| e.to_string())?;
    let executable = executable.canonicalize().map_err(|e| e.to_string())?;
    if !executable.starts_with(&install) {
        return Err("Исполняемый файл находится вне каталога игры".into());
    }
    let arch = architecture(&executable)?;
    let parent = executable
        .parent()
        .ok_or("У исполняемого файла нет каталога")?;
    let mut directories = vec![
        parent.to_path_buf(),
        install.clone(),
        parent.join("bin"),
        install.join("bin"),
    ];
    for relative in [
        "bin/win32",
        "bin/win64",
        "bin/x86",
        "bin/x64",
        "Binaries/Win32",
        "Binaries/Win64",
    ] {
        directories.push(install.join(relative));
    }
    if let Some(stem) = executable.file_stem() {
        let data = parent.join(format!("{}_Data", stem.to_string_lossy()));
        directories.extend([
            data.join("Plugins"),
            data.join("Plugins/x86"),
            data.join("Plugins/x86_64"),
        ]);
    }
    let mut invalid = None;
    for directory in directories {
        let candidate = directory.join(arch.sdk_name());
        if !candidate.exists() {
            continue;
        }
        let path = candidate.canonicalize().map_err(|e| e.to_string())?;
        if !path.starts_with(&install) {
            return Err("Steam API находится вне выбранной установки игры".into());
        }
        match architecture(&path) {
            Ok(actual) if actual == arch => {
                return Ok(Selection {
                    architecture: arch,
                    sdk: Some(path),
                });
            }
            _ => invalid = Some(candidate),
        }
    }
    if let Some(path) = invalid {
        return Err(format!(
            "Steam API не соответствует архитектуре игры {}: {}",
            arch.label(),
            path.display()
        ));
    }
    // Legacy games can link to SteamClient without a redistributable SteamAPI
    // DLL. Their preflight uses the genuine client bridge of the same bitness.
    Ok(Selection {
        architecture: arch,
        sdk: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pe(path: &Path, machine: u16, magic: u16) {
        let mut bytes = vec![0u8; 154];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&machine.to_le_bytes());
        bytes[148..150].copy_from_slice(&2u16.to_le_bytes());
        bytes[152..154].copy_from_slice(&magic.to_le_bytes());
        std::fs::write(path, bytes).unwrap();
    }
    #[test]
    fn selects_legacy32_and_retains_modern64() {
        let dir = tempfile::tempdir().unwrap();
        pe(&dir.path().join("hl.exe"), 0x14c, 0x10b);
        pe(&dir.path().join("steam_api.dll"), 0x14c, 0x10b);
        pe(&dir.path().join("soundpad.exe"), 0x8664, 0x20b);
        pe(&dir.path().join("steam_api64.dll"), 0x8664, 0x20b);
        let old = select(dir.path(), &dir.path().join("hl.exe")).unwrap();
        assert_eq!(old.architecture.probe_name(), "sdk-probe32.exe");
        assert!(old.sdk.unwrap().ends_with("steam_api.dll"));
        let new = select(dir.path(), &dir.path().join("soundpad.exe")).unwrap();
        assert_eq!(new.architecture.probe_name(), "sdk-probe.exe");
        assert!(new.sdk.unwrap().ends_with("steam_api64.dll"));
    }
    #[test]
    fn wrong_architecture_and_malformed_dll_are_not_silently_skipped() {
        let dir = tempfile::tempdir().unwrap();
        pe(&dir.path().join("game.exe"), 0x14c, 0x10b);
        pe(&dir.path().join("steam_api.dll"), 0x8664, 0x20b);
        assert!(select(dir.path(), &dir.path().join("game.exe")).is_err());
        std::fs::write(dir.path().join("steam_api.dll"), b"bad").unwrap();
        assert!(select(dir.path(), &dir.path().join("game.exe")).is_err());
    }
    #[test]
    fn missing_sdk_selects_real_bridge_and_links_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        pe(&dir.path().join("game.exe"), 0x14c, 0x10b);
        assert!(
            select(dir.path(), &dir.path().join("game.exe"))
                .unwrap()
                .sdk
                .is_none()
        );
        pe(&outside.path().join("steam_api.dll"), 0x14c, 0x10b);
        std::os::unix::fs::symlink(
            outside.path().join("steam_api.dll"),
            dir.path().join("steam_api.dll"),
        )
        .unwrap();
        assert!(select(dir.path(), &dir.path().join("game.exe")).is_err());
    }
}
