/* Windows-side proof through the game's unmodified Steamworks DLL, or the
 * pinned NotProton SteamClient bridge when a game has no Steamworks DLL.
 * Both builds use the Windows ABI; no Steam responses are implemented here. */
typedef unsigned long DWORD;
typedef unsigned long long U64;
typedef void *HANDLE;
__declspec(dllimport) HANDLE __stdcall LoadLibraryA(const char *);
__declspec(dllimport) void *__stdcall GetProcAddress(HANDLE, const char *);
__declspec(dllimport) DWORD __stdcall GetEnvironmentVariableA(const char *, char *, DWORD);
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) int __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) U64 __stdcall GetTickCount64(void);
__declspec(dllimport) void __stdcall Sleep(DWORD);
__declspec(dllimport) __declspec(noreturn) void __stdcall ExitProcess(DWORD);

/* Implemented in sdk-client.cpp so CSteamID uses the actual Microsoft C++
 * return ABI (including its hidden return buffer on 32-bit Windows). */
const char *skinos_client_identity(void *, int, int, U64 *, int *);
const char *skinos_bridge_identity(void *, U64 *, int *);

static void out(const char *s) { DWORD n = 0, written; while (s[n]) n++; WriteFile(GetStdHandle((DWORD)-11), s, n, &written, 0); }
static U64 decimal(const char *s) {
    U64 n = 0;
    for (; *s; ++s) {
        if (*s < '0' || *s > '9' || n > 1844674407370955161ULL || (n == 1844674407370955161ULL && *s > '5')) return 0;
        n = n * 10 + *s - '0';
    }
    return n;
}
static void number(U64 n) {
    /* Avoid a 64-bit division CRT dependency in the freestanding x86 build. */
    unsigned char digits[20] = {0};
    char text[21];
    int bit, i, start = 0;
    for (bit = 63; bit >= 0; --bit) {
        unsigned int carry = (unsigned int)((n >> bit) & 1);
        for (i = 19; i >= 0; --i) {
            unsigned int value = digits[i] * 2 + carry;
            digits[i] = (unsigned char)(value % 10);
            carry = value / 10;
        }
    }
    while (start < 19 && !digits[start]) ++start;
    for (i = start; i < 20; ++i) text[i - start] = '0' + digits[i];
    text[20 - start] = 0;
    out(text);
}
static __declspec(noreturn) void fail(const char *reason) { out("{\"event\":\"compat_sdk_proof\",\"ok\":false,\"reason\":\""); out(reason); out("\"}\n"); ExitProcess(1); }

void mainCRTStartup(void) {
    char path[2048], expected[64], message[1024];
    DWORD count = GetEnvironmentVariableA("SKINOS_EXPECTED_STEAMID", expected, sizeof(expected));
    if (!count || count >= sizeof(expected) || !decimal(expected)) fail("missing_expected_identity");
    U64 expected_id = decimal(expected), id = 0;
    int online = 0, sdk_initialized = 0;
    const char *init_method = "SteamClient017";
    const char *readiness_reason = "bridge_sampled";
    U64 readiness_elapsed_ms = 0;
    unsigned int callback_pumps = 0;
    void (*shutdown)(void) = 0;

    count = GetEnvironmentVariableA("SKINOS_WINDOWS_SDK", path, sizeof(path));
    if (count >= sizeof(path)) fail("invalid_sdk_path");
    if (count) {
        HANDLE sdk = LoadLibraryA(path);
        if (!sdk) fail("load_sdk_failed");
        int (*init_flat)(char *) = (void *)GetProcAddress(sdk, "SteamAPI_InitFlat");
        unsigned char (*init_classic)(void) = (void *)GetProcAddress(sdk, "SteamAPI_Init");
        U64 (*get_id)(void *) = (void *)GetProcAddress(sdk, "SteamAPI_ISteamUser_GetSteamID");
        unsigned char (*logged_on)(void *) = (void *)GetProcAddress(sdk, "SteamAPI_ISteamUser_BLoggedOn");
        void (*run_callbacks)(void) = (void *)GetProcAddress(sdk, "SteamAPI_RunCallbacks");
        shutdown = (void *)GetProcAddress(sdk, "SteamAPI_Shutdown");
        if ((!init_flat && !init_classic) || !shutdown) fail("sdk_init_exports_missing");
        if (init_flat) {
            int result = init_flat(message);
            init_method = "SteamAPI_InitFlat";
            if (result != 0) {
                out("{\"event\":\"compat_sdk_proof\",\"ok\":false,\"reason\":\"sdk_init_failed\",\"init_method\":\"SteamAPI_InitFlat\",\"init_result\":");
                number((DWORD)result); out("}\n"); ExitProcess(2);
            }
        } else {
            init_method = "SteamAPI_Init";
            if (!init_classic()) fail("sdk_classic_init_failed");
        }
        sdk_initialized = 1;
        /* Ask only for accessors actually exported by this original DLL. Older
         * Steamworks releases use v021/v019 rather than today's v023. */
        static const char *versions[] = {"SteamAPI_SteamUser_v023", "SteamAPI_SteamUser_v022", "SteamAPI_SteamUser_v021", "SteamAPI_SteamUser_v020", "SteamAPI_SteamUser_v019", "SteamAPI_SteamUser_v018", "SteamAPI_SteamUser_v017", "SteamAPI_SteamUser_v016"};
        void *user = 0;
        unsigned int i;
        if (get_id && logged_on) {
            for (i = 0; i < sizeof(versions) / sizeof(versions[0]); ++i) {
                void *(*get_user)(void) = (void *)GetProcAddress(sdk, versions[i]);
                if (get_user && (user = get_user())) break;
            }
        }
        void *(*get_client)(void) = (void *)GetProcAddress(sdk, "SteamClient");
        int (*get_user_handle)(void) = (void *)GetProcAddress(sdk, "SteamAPI_GetHSteamUser");
        int (*get_pipe_handle)(void) = (void *)GetProcAddress(sdk, "SteamAPI_GetHSteamPipe");
        if (!user && (!get_client || !get_user_handle || !get_pipe_handle)) {
            shutdown(); fail("sdk_user_exports_missing");
        }
        /* Successful Init does not itself prove an online account. Process
         * the original SDK's callbacks on this owner thread, then resample
         * the real interfaces. Never wait through a different nonzero ID.
         * Valve's documented BLoggedOn can legitimately remain false while
         * disconnected; expiration must retain that failed proof. */
        const U64 started = GetTickCount64();
        for (;;) {
            if (user) {
                id = get_id(user);
                online = logged_on(user);
            } else {
                const char *error = skinos_client_identity(get_client(), get_user_handle(), get_pipe_handle(), &id, &online);
                if (error) { shutdown(); fail(error); }
            }
            readiness_elapsed_ms = GetTickCount64() - started;
            if (id && id != expected_id) { readiness_reason = "identity_mismatch"; break; }
            if (id == expected_id && online) { readiness_reason = "ready"; break; }
            if (readiness_elapsed_ms >= 10000) { readiness_reason = "sdk_readiness_timeout"; break; }
            if (!run_callbacks) { readiness_reason = "sdk_callbacks_export_missing"; break; }
            run_callbacks();
            ++callback_pumps;
            U64 remaining = 10000 - readiness_elapsed_ms;
            Sleep((DWORD)(remaining < 25 ? remaining : 25));
        }
    } else {
        /* Explicit fallback for legacy games without a SteamAPI DLL. This
         * proves the Windows bridge, not initialization of a nonexistent SDK.
         * A supplied but broken game DLL never falls back to this path. */
        count = GetEnvironmentVariableA("SKINOS_WINDOWS_CLIENT", path, sizeof(path));
        if (!count || count >= sizeof(path)) fail("missing_sdk_or_client_path");
        HANDLE client = LoadLibraryA(path);
        if (!client) fail("load_steamclient_failed");
        void *factory = GetProcAddress(client, "CreateInterface");
        if (!factory) fail("steamclient_factory_missing");
        const char *error = skinos_bridge_identity(factory, &id, &online);
        if (error) fail(error);
    }
    int matches = id == expected_id && id != 0;
    int ok = online && matches;
    out("{\"event\":\"compat_sdk_proof\",\"ok\":"); out(ok ? "true" : "false");
    out(sdk_initialized ? ",\"proof_kind\":\"game_sdk\",\"sdk_initialized\":true" : ",\"proof_kind\":\"steamclient_bridge\",\"bridge_initialized\":true");
    out(",\"init_method\":\""); out(init_method); out("\",\"expected_matches\":"); out(matches ? "true" : "false");
    out(",\"logged_on\":"); out(online ? "true" : "false");
    out(",\"readiness_reason\":\""); out(readiness_reason); out("\"");
    out(",\"readiness_elapsed_ms\":"); number(readiness_elapsed_ms);
    out(",\"callback_pumps\":"); number(callback_pumps);
    out(",\"steamid\":\""); number(id); out("\"}\n");
    if (shutdown) shutdown();
    ExitProcess(ok ? 0 : 3);
}
