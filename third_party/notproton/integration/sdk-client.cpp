/* Minimal ABI declarations for a real SteamClient connection. Slot order and
 * CSteamID layout are from Valve's Steamworks 1.42 headers, pinned by NotProton:
 * ValveSoftware/Proton@164e0ccd2ea2b1ec2e5d08dc97f65b184c2539ca/lsteamclient/
 * steamworks_sdk_142/{isteamclient.h,isteamuser.h,steamclientpublic.h}.
 * This file calls the bridge; it does not implement Steam interfaces. */
typedef unsigned long long U64;
#pragma pack(push, 1)
class SteamId {
    U64 bits;
public:
    SteamId() : bits(0) {}
    U64 value() const { return bits; }
};
#pragma pack(pop)
static_assert(sizeof(SteamId) == 8, "CSteamID ABI must be 64 bits");
class SteamUserPrefix {
public:
    virtual int GetHSteamUser() = 0;
    virtual bool BLoggedOn() = 0;
    virtual SteamId GetSteamID() = 0;
};
class SteamClientPrefix {
public:
    virtual int CreateSteamPipe() = 0;
    virtual bool BReleaseSteamPipe(int) = 0;
    virtual int ConnectToGlobalUser(int) = 0;
    virtual int CreateLocalUser(int *, int) = 0;
    virtual void ReleaseUser(int, int) = 0;
    virtual SteamUserPrefix *GetISteamUser(int, int, const char *) = 0;
};
extern "C" const char *skinos_client_identity(void *raw, int user, int pipe, U64 *id, int *online) {
    if (!raw || !user || !pipe) return "steamclient_connection_missing";
    auto client = static_cast<SteamClientPrefix *>(raw);
    auto iface = client->GetISteamUser(user, pipe, "SteamUser019");
    if (!iface) return "steam_user_missing";
    *id = iface->GetSteamID().value();
    *online = iface->BLoggedOn();
    return nullptr;
}
extern "C" const char *skinos_bridge_identity(void *raw, U64 *id, int *online) {
    auto factory = reinterpret_cast<void *(*)(const char *, int *)>(raw);
    auto client = static_cast<SteamClientPrefix *>(factory("SteamClient017", nullptr));
    if (!client) return "steamclient_interface_missing";
    int pipe = client->CreateSteamPipe();
    if (!pipe) return "steamclient_pipe_failed";
    int user = client->ConnectToGlobalUser(pipe);
    const char *error = skinos_client_identity(client, user, pipe, id, online);
    if (user) client->ReleaseUser(pipe, user);
    client->BReleaseSteamPipe(pipe);
    return error;
}
