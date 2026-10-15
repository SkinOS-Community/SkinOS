# Counter-Strike null read at 0x0040107F — fixed and verified

Static diagnosis on 2026-10-09 of the installed original `Half-Life/hl.exe` (SHA-256 `be7c061a2d36a3517c98412a06642f98985ab746e3d65c8d105d25b6ac77369c`). This audit did not start/stop Wine, Steam or a game, and did not modify their files or prefixes.

The supplied `probe-80587-1791507653933447000` log shows the new x86 preflight successfully calling the game's genuine legacy `SteamAPI_Init`, returning the expected online identity. The game then faults while resolving its executable path. A transient process/Steam game registration did not establish a working game window.

## Exact failing code

LLVM disassembly and PE import-address table resolve the following sequence in the original `hl.exe`:

```c
char temporary[MAX_PATH] = {0};
global_path[0] = 0;
if (GetModuleFileNameA(NULL, temporary, MAX_PATH)) {
    GetLongPathNameA(temporary, global_path, MAX_PATH); // result unchecked
    char *separator = strrchr(global_path, '\\');
    if (*separator != 0) separator[1] = 0;             // crash if null
    // Trim trailing slash and return global_path.
}
```

| Address | Verified instruction / import |
|---|---|
| `0x0040104F` | Indirect call via IAT `0x00404028`: `GetModuleFileNameA` |
| `0x0040106A` | Indirect call via IAT `0x00404060`: `GetLongPathNameA` |
| `0x00401077` | Call thunk `0x004035E5`, IAT `0x004040AC`: `strrchr` |
| `0x0040107F` | `cmpb $0x0, (%eax)`; EAX is the unchecked `strrchr` result |

Consequently this exact crash is **not an indirect call into a missing hw.dll or Steam DLL**. The immediate cause is that the executable-path string has no backslash. The subsequent controlled probe below confirmed that the sandbox caused `GetLongPathNameA` to return zero and leave an empty output.

## Confirmed directory-enumeration dependency

The failed profile grants file data reads below the selected game and below `steamapps`, but does not allow listing their ancestor directories. Wine's [upstream `GetLongPathNameW`](https://github.com/wine-mirror/wine/blob/master/dlls/kernelbase/file.c#L1963) walks path components and calls `FindFirstFileW` on each accumulated path. A failed enumeration returns zero; [the A wrapper](https://github.com/wine-mirror/wine/blob/master/dlls/kernelbase/file.c#L1940) propagates it. This source explains the dependency; it is not asserted to be byte-identical to CrossOver's Wine.

Missing **literal directory** read allowances for the Z: path in this installation:

```text
/Users
$HOME
$HOME/Library
$HOME/Library/Application Support
$HOME/Library/Application Support/Steam
```

The profile already allows `/` and the `steamapps` subtree. The existing Wine prefix maps S: to the Steam directory and Z: to `/`. The controlled probe compared the unchanged strict policy, one added literal Steam-root directory grant, and the literal ancestor chain. [Raw A/B report](../../outputs/goldsrc-path-policy-check.json):

| Policy | S: game path | Z: game path |
|---|---|---|
| Original strict policy | 0, error 2, empty output | 0, error 2, empty output |
| Literal Steam-root listing | 36 characters, error 0 | 0, error 2, empty output |
| Literal ancestor listings | 36 characters, error 0 | 81 characters, error 0 |

The game executable and SDK were unchanged between variants. This establishes the missing directory-enumeration dependency independently of Steam authentication. The production fix derives literal directory ancestors from paths already allowed by the compatibility plan plus its own runtime/profile roots, covering both drive mappings. It grants no recursive access to all of Steam, the user's home or Application Support. The actual `sandbox-exec` regression test in `crates/runtime-host/src/main.rs` permits listing the ancestor and still denies reading `Steam/private.fixture`. The final runtime also passes the original protected-file, write and alias guards.

The companion log message `Could not convert "$HOME/Library/Application Support" to win path` comes from NotProton's [`setup_steam_files`](https://github.com/NotProtonNot/NotProton/blob/bd0fcaa22e0ea28ac67d861fc1c4e7611207f696/steam-shim/steam.cpp#L1560), which calls `wine_get_dos_file_name`. It is consistent with an ancestor-path issue, but does not itself prove that the specific `GetLongPathNameA` call failed.

## Verification artifacts and result

`outputs/goldsrc-path-probe.c` and the compiled i386 `outputs/goldsrc-path-probe-build/path-probe32.exe` perform read-only Win32 path calls. They print `GetModuleFileNameA`, `GetLongPathNameA` return values, last-error codes, and output paths for the probe, an optional `SKINOS_PATH_PROBE_TARGET`, and the ancestors above. The diagnostic never initializes Steam or invokes the game. The binary was built with Clang/Rust LLD; the root agent ran the controlled prefix/sandbox comparisons recorded above.

The initial approximately three-second launch/stop check is superseded as evidence for a working game: it stopped after process registration and missed the later crash. After the directory fix, the candidate app's bundled core ran Counter-Strike for **62.94 seconds**, and the user confirmed the normal menu. No fatal Wine diagnostic or original Steam GUI was observed. The requested stop completed with runtime exit code 0, no timeout, and clean release. The [stability report](../../outputs/counterstrike-stability-check.json) records the duration, confirmation and process observation; the [runtime proof](../../outputs/counterstrike-stable-runtime-proof.json) records the genuine SDK, sandbox guards and cleanup. The corrected app is installed at `dist/SkinOS.app`; original game binaries remain unchanged.

Runtime monitoring now recognizes actual Wine page-fault/unhandled-exception diagnostics and reports `failed`, preserving cleanup even if the game previously reached `running`. A later Stop request cannot turn an observed crash into a successful shutdown. Benign host warnings are excluded by regression tests. This prevents the supplied failure from being summarized as an ordinary exit again.

The verified scope is startup through the normal menu, more than one minute of observation, and requested shutdown. A network match, every graphics backend and long-session stability were not established by this check.
