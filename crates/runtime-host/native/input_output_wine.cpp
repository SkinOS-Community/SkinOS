// Explicit alternative output transport for our OWN Wine runtime. This is not
// an OS HID implementation and never calls IOHID. It consumes the genuine Valve
// engine's mapped output; no Steamworks account/API/handle state is synthesized.
#include "steam_input_output.h"
#include "input_output_wire.h"
#include <cerrno>
#include <climits>
#include <cstdlib>
#include <cstring>
#include <cstdio>
#include <fcntl.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define SKINOS_EXPORT extern "C" __attribute__((visibility("default")))

namespace {
constexpr uint8_t kDescriptor[SKINOS_VALVE_GAMEPAD_DESCRIPTOR_BYTES] = {
    0x05,0x01,0x09,0x05,0xa1,0x01,0x05,0x01,0x09,0x3a,0xa1,0x02,0x75,0x08,0x95,0x02,
    0x05,0x01,0x09,0x3f,0x09,0x3b,0x81,0x01,0x75,0x01,0x15,0x00,0x25,0x01,0x35,0x00,
    0x45,0x01,0x95,0x04,0x05,0x09,0x19,0x0c,0x29,0x0f,0x81,0x02,0x75,0x01,0x15,0x00,
    0x25,0x01,0x35,0x00,0x45,0x01,0x95,0x04,0x05,0x09,0x09,0x09,0x09,0x0a,0x09,0x07,
    0x09,0x08,0x81,0x02,0x75,0x01,0x15,0x00,0x25,0x01,0x35,0x00,0x45,0x01,0x95,0x03,
    0x05,0x09,0x09,0x05,0x09,0x06,0x09,0x0b,0x81,0x02,0x75,0x01,0x95,0x01,0x81,0x01,
    0x75,0x01,0x15,0x00,0x25,0x01,0x35,0x00,0x45,0x01,0x95,0x04,0x05,0x09,0x19,0x01,
    0x29,0x04,0x81,0x02,0x75,0x08,0x15,0x00,0x26,0xff,0x00,0x35,0x00,0x46,0xff,0x00,
    0x95,0x02,0x05,0x01,0x09,0x32,0x09,0x35,0x81,0x02,0x75,0x10,0x16,0x00,0x80,0x26,
    0xff,0x7f,0x36,0x00,0x80,0x46,0xff,0x7f,0x05,0x01,0x09,0x01,0xa1,0x00,0x95,0x02,
    0x05,0x01,0x09,0x30,0x09,0x31,0x81,0x02,0xc0,0x05,0x01,0x09,0x01,0xa1,0x00,0x95,
    0x02,0x05,0x01,0x09,0x33,0x09,0x34,0x81,0x02,0xc0,0xc0,0xc0
};
// Exact bytes extracted from the pinned engine's descriptor at 0x17e8ba8;
// SHA256 is SKINOS_VALVE_GAMEPAD_DESCRIPTOR_SHA256. Byte equality is stronger
// than hashing again and avoids accepting arbitrary HID descriptors.

pthread_mutex_t gMutex = PTHREAD_MUTEX_INITIALIZER;
bool gActive[SKINOS_INPUT_WIRE_SLOTS] = {};
SkinosInputOutputStats gStats = {1, sizeof(SkinosInputOutputStats), 0, 0, 0, 0, 0, 0};
struct Lock {
    bool held = pthread_mutex_lock(&gMutex) == 0;
    ~Lock() { if (held) pthread_mutex_unlock(&gMutex); }
};

bool exact_env(const char *key, const char *value) {
    const char *actual = std::getenv(key);
    return actual && std::strcmp(actual, value) == 0;
}

int configured_fd() {
    if (!exact_env("SKINOS_COMPONENT_ROLE", "runtime")
        || !exact_env("SKINOS_INPUT_OUTPUT_BACKEND", "wine")) return -1;
    const char *value = std::getenv("SKINOS_INPUT_OUTPUT_FD");
    if (!value || !*value) return -1;
    unsigned number = 0;
    for (const char *p = value; *p; ++p) {
        if (*p < '0' || *p > '9' || number > (INT_MAX - unsigned(*p - '0')) / 10u) return -1;
        number = number * 10u + unsigned(*p - '0');
    }
    if (number < 3) return -1;
    const int fd = static_cast<int>(number);
    const int flags = fcntl(fd, F_GETFD);
    if (flags < 0 || !(flags & FD_CLOEXEC)) return -1;
    int type = 0;
    socklen_t length = sizeof(type);
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &length) != 0 || type != SOCK_DGRAM) return -1;
    sockaddr_un local{}, peer{};
    length = sizeof(local);
    if (getsockname(fd, reinterpret_cast<sockaddr *>(&local), &length) != 0
        || local.sun_family != AF_UNIX) return -1;
    length = sizeof(peer);
    if (getpeername(fd, reinterpret_cast<sockaddr *>(&peer), &length) != 0
        || peer.sun_family != AF_UNIX) return -1;
    return fd;
}

int slot_for(const char *product) {
    if (!product || std::strncmp(product, "GamePad-", 8) != 0) return -1;
    const char *suffix = product + 8;
    // Only canonical engine product keys GamePad-1 .. GamePad-16.
    if (suffix[0] >= '1' && suffix[0] <= '9' && suffix[1] == '\0') return suffix[0] - '1';
    if (suffix[0] == '1' && suffix[1] >= '0' && suffix[1] <= '6' && suffix[2] == '\0')
        return 9 + suffix[1] - '0';
    return -1;
}

void write_word(uint8_t *frame, unsigned word, uint32_t value) {
    for (unsigned byte = 0; byte < 4; ++byte) frame[word * 4 + byte] = uint8_t(value >> (byte * 8));
}

bool queue_frame(int fd, uint32_t kind, uint32_t slot, const uint8_t *payload, uint32_t length) {
    uint8_t frame[SKINOS_INPUT_WIRE_FRAME_BYTES] = {};
    write_word(frame, SKINOS_INPUT_WORD_MAGIC, SKINOS_INPUT_WIRE_MAGIC);
    write_word(frame, SKINOS_INPUT_WORD_VERSION, SKINOS_INPUT_WIRE_VERSION);
    write_word(frame, SKINOS_INPUT_WORD_KIND, kind);
    write_word(frame, SKINOS_INPUT_WORD_SLOT, slot);
    write_word(frame, SKINOS_INPUT_WORD_PAYLOAD_LENGTH, length);
    write_word(frame, SKINOS_INPUT_WORD_VENDOR, SKINOS_VALVE_GAMEPAD_VENDOR_ID);
    write_word(frame, SKINOS_INPUT_WORD_PRODUCT, SKINOS_VALVE_GAMEPAD_PRODUCT_ID);
    write_word(frame, SKINOS_INPUT_WORD_DEVICE_VERSION, 0);
    if (length) std::memcpy(frame + SKINOS_INPUT_WIRE_HEADER_BYTES, payload, length);
    int options = MSG_DONTWAIT;
#ifdef MSG_NOSIGNAL
    options |= MSG_NOSIGNAL;
#endif
    // A datagram either queues whole or fails. Never block, retry indefinitely,
    // fabricate an ACK, truncate a report, or interpret queued as Wine delivery.
    const ssize_t sent = fd >= 0 ? send(fd, frame, sizeof(frame), options) : -1;
    const int saved_errno = errno;
    const bool queued = sent == static_cast<ssize_t>(sizeof(frame));
    if (queued) ++gStats.queued_frames;
    else {
        ++gStats.failed_frames; ++gStats.loss_generation;
        if (exact_env("SKINOS_INPUT_TRACE", "1")) {
            // Transport metadata only. Never log an input report or identity.
            std::fprintf(stderr, "SKINOS_INPUT_OUTPUT queue_failed kind=%u slot=%u fd_valid=%d sent=%ld errno=%d\n",
                kind, slot, fd >= 0, static_cast<long>(sent), fd >= 0 ? saved_errno : 0);
        }
    }
    errno = saved_errno;
    return queued;
}
}

SKINOS_EXPORT bool SteamInput_Available() { return configured_fd() >= 0; }
SKINOS_EXPORT bool SteamInput_IsVersionNewerThanKextVersion() { return false; }
SKINOS_EXPORT bool SkinosInputOutput_GetStats(SkinosInputOutputStats *output, uint32_t bytes) {
    if (!output || bytes != sizeof(*output)) return false;
    Lock lock;
    if (!lock.held) return false;
    *output = gStats;
    return true;
}

SKINOS_EXPORT bool SteamInput_Create(
    const char *product, const char *manufacturer, const char *serial,
    uint32_t vendor, uint32_t product_id, uint32_t device_version,
    const uint8_t *descriptor, intptr_t descriptor_bytes) {
    const int slot = slot_for(product);
    const int fd = configured_fd();
    if (fd < 0 || slot < 0 || !manufacturer || !serial || !descriptor
        || std::strcmp(manufacturer, "Microsoft") != 0 || std::strcmp(serial, "A96BA260") != 0
        || vendor != SKINOS_VALVE_GAMEPAD_VENDOR_ID || product_id != SKINOS_VALVE_GAMEPAD_PRODUCT_ID
        || device_version != 0 || descriptor_bytes != sizeof(kDescriptor)
        || std::memcmp(descriptor, kDescriptor, sizeof(kDescriptor)) != 0) return false;
    Lock lock;
    if (!lock.held) return false;
    if (gActive[slot]) {
        // Original CFDictionaryAddValue does not replace an existing key.
        // Never send a second Create which our receiver cannot safely apply.
        // A repeated request may intend a state reset, so treat it as a loss
        // requiring receiver disconnect, rather than silently holding old state.
        ++gStats.rejected_active_creates; ++gStats.failed_frames; ++gStats.loss_generation;
        return false;
    }
    if (!queue_frame(fd, SKINOS_INPUT_WIRE_CREATE, unsigned(slot), descriptor, sizeof(kDescriptor))) return false;
    gActive[slot] = true;
    gStats.active_slot_mask |= uint32_t(1u << slot);
    return true; // queued to the explicitly selected Wine backend, not OS HID
}

SKINOS_EXPORT bool SteamInput_Send(const char *product, const uint8_t *report, intptr_t bytes) {
    const int slot = slot_for(product);
    const int fd = configured_fd();
    if (slot < 0 || !report || bytes != SKINOS_VALVE_GAMEPAD_REPORT_BYTES) return false;
    Lock lock;
    return lock.held && gActive[slot]
        && queue_frame(fd, SKINOS_INPUT_WIRE_REPORT, unsigned(slot), report, unsigned(bytes));
}

SKINOS_EXPORT bool SteamInput_Destroy(const char *product) {
    const int slot = slot_for(product);
    const int fd = configured_fd();
    if (slot < 0) return false;
    Lock lock;
    if (!lock.held || !gActive[slot] || !queue_frame(fd, SKINOS_INPUT_WIRE_DESTROY, unsigned(slot), nullptr, 0)) return false;
    gActive[slot] = false;
    gStats.active_slot_mask &= ~uint32_t(1u << slot);
    return true;
}

#ifdef SKINOS_INPUT_OUTPUT_TEST
#include <cassert>
#include <cstdio>
#include <initializer_list>
namespace {
struct Endpoint {
    int pair[2] = {-1, -1};
    Endpoint() {
        assert(socketpair(AF_UNIX, SOCK_DGRAM, 0, pair) == 0);
        assert(fcntl(pair[0], F_SETFD, FD_CLOEXEC) == 0);
        char value[32];std::snprintf(value, sizeof(value), "%d", pair[0]);
        setenv("SKINOS_INPUT_OUTPUT_FD", value, 1);
        setenv("SKINOS_COMPONENT_ROLE", "runtime", 1);
        setenv("SKINOS_INPUT_OUTPUT_BACKEND", "wine", 1);
        std::memset(gActive, 0, sizeof(gActive));
        gStats = {1, sizeof(SkinosInputOutputStats), 0, 0, 0, 0, 0, 0};
    }
    ~Endpoint() { for(int fd:pair) if(fd>=0)close(fd); }
    void empty() { uint8_t byte; assert(recv(pair[1], &byte, 1, MSG_DONTWAIT) == -1 && (errno==EAGAIN || errno==EWOULDBLOCK)); }
    void read(uint8_t (&frame)[SKINOS_INPUT_WIRE_FRAME_BYTES]) {
        assert(recv(pair[1],frame,sizeof(frame),MSG_DONTWAIT)==sizeof(frame));
    }
};
bool create(const char *name="GamePad-1",const uint8_t *d=kDescriptor,intptr_t len=sizeof(kDescriptor)) {
    return SteamInput_Create(name,"Microsoft","A96BA260",0x045e,0x028e,0,d,len);
}
void wire_bytes() {
    Endpoint endpoint; assert(SteamInput_Available());assert(create());
    uint8_t frame[SKINOS_INPUT_WIRE_FRAME_BYTES];endpoint.read(frame);
    const uint32_t expected[8]={0x53494e50,1,1,0,188,0x045e,0x028e,0};
    for(unsigned word=0;word<8;++word)assert(skinos_input_wire_u32(frame+word*4)==expected[word]);
    assert(frame[0]==0x50&&frame[1]==0x4e&&frame[2]==0x49&&frame[3]==0x53);
    assert(std::memcmp(frame+32,kDescriptor,188)==0);
    for(unsigned i=220;i<288;++i)assert(frame[i]==0);
    // Test fixture bytes are arbitrary; transport must not reinterpret them.
    uint8_t report[20];for(unsigned i=0;i<20;++i)report[i]=uint8_t(i*7);
    assert(SteamInput_Send("GamePad-1",report,20));endpoint.read(frame);
    assert(skinos_input_wire_u32(frame+8)==2&&skinos_input_wire_u32(frame+16)==20);
    assert(std::memcmp(frame+32,report,20)==0);
    assert(SteamInput_Destroy("GamePad-1"));endpoint.read(frame);
    assert(skinos_input_wire_u32(frame+8)==3&&skinos_input_wire_u32(frame+16)==0);
    assert(!SteamInput_Send("GamePad-1",report,20));endpoint.empty();
}
void rejects_inputs() {
    Endpoint e;uint8_t bad[188];std::memcpy(bad,kDescriptor,188);bad[187]^=1;
    for(const char *name:{"GamePad-0","GamePad-17","GamePad-01","GamePad-1junk","Other-1","GamePad-"})assert(!create(name));
    assert(!create(nullptr));assert(!create("GamePad-1",bad));assert(!create("GamePad-1",kDescriptor,187));
    assert(!create("GamePad-1",kDescriptor,-1));
    assert(!SteamInput_Create("GamePad-1","Microsoft","A96BA260",1,0x028e,0,kDescriptor,188));
    uint8_t report[20]={};assert(!SteamInput_Send("GamePad-1",report,20));
    assert(create("GamePad-16"));uint8_t f[288];e.read(f);assert(skinos_input_wire_u32(f+12)==15);
    assert(!SteamInput_Send("GamePad-16",report,19));assert(!SteamInput_Send("GamePad-16",nullptr,20));e.empty();
}
void rejects_transport() {
    Endpoint e;unsetenv("SKINOS_INPUT_OUTPUT_BACKEND");assert(!SteamInput_Available());assert(!create());
    setenv("SKINOS_INPUT_OUTPUT_BACKEND","wine",1);setenv("SKINOS_COMPONENT_ROLE","broker",1);assert(!create());
    setenv("SKINOS_COMPONENT_ROLE","runtime",1);assert(fcntl(e.pair[0],F_SETFD,0)==0);assert(!create());
    assert(fcntl(e.pair[0],F_SETFD,FD_CLOEXEC)==0);e.empty();
    setenv("SKINOS_INPUT_OUTPUT_FD","99999999999999999999",1);assert(!create());
}
void queue_failure_does_not_activate() {
    Endpoint e;
    // Fill the socket with bounded own datagrams until nonblocking send fails.
    uint8_t junk[288]={};unsigned count=0;
    while(send(e.pair[0],junk,sizeof(junk),MSG_DONTWAIT)==sizeof(junk)){assert(++count<100000);}
    assert(errno==EAGAIN||errno==EWOULDBLOCK||errno==ENOBUFS);
    assert(!create());assert(!gActive[0]);
    SkinosInputOutputStats stats{};assert(SkinosInputOutput_GetStats(&stats,sizeof(stats)));
    assert(stats.failed_frames==1&&stats.loss_generation==1&&stats.active_slot_mask==0);
}
void wrong_socket_type_and_closed_peer() {
    Endpoint e;int stream[2];assert(socketpair(AF_UNIX,SOCK_STREAM,0,stream)==0);
    assert(fcntl(stream[0],F_SETFD,FD_CLOEXEC)==0);
    char value[32];std::snprintf(value,sizeof(value),"%d",stream[0]);
    setenv("SKINOS_INPUT_OUTPUT_FD",value,1);assert(!SteamInput_Available());assert(!create());
    close(stream[0]);close(stream[1]);
    std::snprintf(value,sizeof(value),"%d",e.pair[0]);setenv("SKINOS_INPUT_OUTPUT_FD",value,1);
    close(e.pair[1]);e.pair[1]=-1;assert(!create());assert(!gActive[0]);
}
void duplicate_create_is_explicit_loss_without_duplicate_frame() {
    Endpoint e;assert(create());uint8_t frame[288];e.read(frame);assert(!create());e.empty();
    SkinosInputOutputStats stats{};assert(SkinosInputOutput_GetStats(&stats,sizeof(stats)));
    assert(stats.queued_frames==1&&stats.rejected_active_creates==1&&stats.loss_generation==1);
    assert(!SkinosInputOutput_GetStats(&stats,sizeof(stats)-1));
}
}
int main(){wire_bytes();rejects_inputs();rejects_transport();queue_failure_does_not_activate();wrong_socket_type_and_closed_peer();duplicate_create_is_explicit_loss_without_duplicate_frame();std::puts("input output backend: 6 offline tests passed");}
#endif
