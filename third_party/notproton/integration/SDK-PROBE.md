# Windows Steam verification

`build_sdk_probes()` builds `sdk-probe32.exe` (PE32/i386) and `sdk-probe.exe`
(PE32+/AMD64). The runtime selects the width using the game's PE executable and
loads the matching **unchanged game DLL** through NotProton. Win32 imports retain
their stdcall ABI while referring to the undecorated names exported by kernel32.

The probe uses NotProton's `waitforexitandrun`, just like the game, so its bundled
Windows `steam.exe` shim initializes Steam registry paths and process metadata
before the original SDK is loaded. `runinprefix` bypasses that bootstrap and can
fail on a newly created prefix even when the same SDK succeeds in an older,
already initialized prefix. This was reproduced with Raft's x64 classic SDK:
`SteamAPI_Init` failed before any game process started, while the native runtime
had already authenticated and confirmed ownership. The foreground shim also
preserves the proof's stdout and exit status. It is the compatibility launcher;
the official macOS Steam GUI is not started.

`SKINOS_WINDOWS_SDK` supplies the absolute Windows path to that DLL. The probe
uses `SteamAPI_InitFlat` when available, or the older `SteamAPI_Init`, then an
exported SteamUser accessor. In particular, the installed GoldSrc `steam_api.dll`
is x86 and exports `SteamAPI_Init` and `SteamAPI_SteamUser_v021`. Requiring x64,
InitFlat and v023 rejected it before the actual game could start.

An older DLL without flat SteamUser accessors can expose its initialized client,
pipe and user through `SteamClient`, `SteamAPI_GetHSteamPipe` and
`SteamAPI_GetHSteamUser`. `sdk-client.cpp` reads the real interface using the
Microsoft C++ ABI, including CSteamID's hidden return buffer on x86. The minimal
prefix declarations follow Valve's headers pinned by NotProton:

- [ISteamClient](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_142/isteamclient.h)
- [ISteamUser](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_142/isteamuser.h)
- [CSteamID](https://github.com/ValveSoftware/Proton/blob/164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/steamworks_sdk_142/steamclientpublic.h)

For a game **without** a SteamAPI DLL, `SKINOS_WINDOWS_CLIENT` can explicitly
select the pinned `C:\Program Files (x86)\Steam\steamclient.dll` or
`steamclient64.dll`. The probe creates a pipe, connects to the actual global user,
reads SteamUser019, and releases both handles. Its report says
`proof_kind: steamclient_bridge`, `bridge_initialized: true`; it never claims
that a nonexistent game SDK initialized. A supplied but broken game DLL cannot
fall back to this branch.

Every positive report requires a nonzero SteamID from the real interface equal
to `SKINOS_EXPECTED_STEAMID`, plus `BLoggedOn == true`. Authentication and app
subscription are separately required by the owning runtime. Offline tests are
explicit fixtures, never packaged or accepted as runtime verification:

```sh
python3 -m unittest scripts.tests.test_sdk_probe -v
```

These tests exercise classic and modern negotiation, identity mismatch, offline
and failed initialization, absence of fallback after a broken DLL, exact proof
classification, and the actual built binaries' PE width/import table. They do
not replace testing the real game through the signed SkinOS bundle.
