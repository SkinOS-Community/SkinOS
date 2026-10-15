/* Isolated Win32/SDK fixtures for the probe decision tree. This is never
 * packaged, connected to Steam, or used as launch verification. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define __declspec(value)
#define __stdcall
#include "../../../third_party/notproton/integration/sdk-probe.c"

static int initialized, shut_down, bridge_calls, callback_pumps;
static U64 clock_ms;
static const char *mode(void) { return getenv("TEST_CASE"); }
static U64 fixture_id(void) { return 76561198000000001ULL; }
static int init_flat(char *message) { (void)message; initialized = 1; return 0; }
static unsigned char init_classic(void) { initialized = 1; return strcmp(mode(), "init_failure") != 0; }
static void shutdown_fixture(void) { shut_down = 1; }
static void *user_fixture(void) { if (!initialized) abort(); return (void *)1; }
static U64 id_fixture(void *user) {
    if (!user) abort();
    if (!strcmp(mode(), "identity_missing")) return 0;
    if (!strcmp(mode(), "identity_delayed") && callback_pumps < 3) return 0;
    if (!strcmp(mode(), "wrong_after_callback") && callback_pumps) return fixture_id() + 1;
    return fixture_id();
}
static unsigned char online_fixture(void *user) {
    if (!user) abort();
    if (!strcmp(mode(), "offline") || !strcmp(mode(), "callbacks_missing") || !strcmp(mode(), "wrong_after_callback")) return 0;
    if (!strcmp(mode(), "online_delayed") || !strcmp(mode(), "identity_delayed") || !strcmp(mode(), "client_delayed")) return callback_pumps >= 3;
    return 1;
}
static void callbacks_fixture(void) { if (!initialized || shut_down) abort(); ++callback_pumps; }
static void *client_fixture(void) { return (void *)2; }
static int handle_fixture(void) { return 1; }
U64 GetTickCount64(void) { return clock_ms; }
void Sleep(DWORD delay) { if (!delay || delay > 25) abort(); clock_ms += delay; }
HANDLE LoadLibraryA(const char *path) { return strcmp(path, "broken.dll") ? (void *)1 : 0; }
void *GetProcAddress(HANDLE library, const char *name) {
    (void)library;
    if (!strcmp(name, "SteamAPI_InitFlat") && strcmp(mode(), "classic") && strcmp(mode(), "init_failure") && strcmp(mode(), "client_delayed")) return (void *)init_flat;
    if (!strcmp(name, "SteamAPI_Init")) return (void *)init_classic;
    if (!strcmp(name, "SteamAPI_Shutdown")) return (void *)shutdown_fixture;
    if (!strcmp(name, "SteamAPI_RunCallbacks") && strcmp(mode(), "callbacks_missing")) return (void *)callbacks_fixture;
    if (!strcmp(name, "SteamAPI_ISteamUser_GetSteamID") && strcmp(mode(), "client_delayed")) return (void *)id_fixture;
    if (!strcmp(name, "SteamAPI_ISteamUser_BLoggedOn")) return (void *)online_fixture;
    if (!strcmp(name, !strcmp(mode(), "modern") ? "SteamAPI_SteamUser_v023" : "SteamAPI_SteamUser_v021")) return (void *)user_fixture;
    if (!strcmp(name, "SteamClient")) return (void *)client_fixture;
    if (!strcmp(name, "SteamAPI_GetHSteamUser") || !strcmp(name, "SteamAPI_GetHSteamPipe")) return (void *)handle_fixture;
    if (!strcmp(name, "CreateInterface")) return (void *)1;
    return 0;
}
DWORD GetEnvironmentVariableA(const char *name, char *result, DWORD capacity) {
    const char *value = getenv(name);
    if (!value) return 0;
    DWORD size = strlen(value);
    if (size < capacity) memcpy(result, value, size + 1);
    return size;
}
HANDLE GetStdHandle(DWORD handle) { (void)handle; return stdout; }
int WriteFile(HANDLE output, const void *bytes, DWORD count, DWORD *written, void *overlapped) {
    (void)overlapped; *written = fwrite(bytes, 1, count, output); return 1;
}
void ExitProcess(DWORD status) {
    if (initialized && strcmp(mode(), "init_failure") && !shut_down) abort();
    if (!strcmp(mode(), "broken") && bridge_calls) abort();
    exit((int)status);
}
const char *skinos_client_identity(void *client, int user, int pipe, U64 *id, int *online) {
    if (!client || !user || !pipe || strcmp(mode(), "client_delayed")) abort();
    *id = id_fixture(client); *online = online_fixture(client); return 0;
}
const char *skinos_bridge_identity(void *factory, U64 *id, int *online) {
    if (!factory) abort();
    bridge_calls++;
    *id = fixture_id(); *online = 1;
    return 0;
}
int main(void) { mainCRTStartup(); }
