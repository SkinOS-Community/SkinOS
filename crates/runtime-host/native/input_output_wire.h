#ifndef SKINOS_INPUT_OUTPUT_WIRE_H
#define SKINOS_INPUT_OUTPUT_WIRE_H
#include <stddef.h>
#include <stdint.h>

// One complete connected UNIX datagram per frame. All eight words are LE.
// Create's payload is the actual Valve descriptor; report's payload is the
// unmodified Valve-mapped 20-byte report. No account or Steam handle is present.
enum {
    SKINOS_INPUT_WIRE_MAGIC = 0x53494e50,
    SKINOS_INPUT_WIRE_VERSION = 1,
    SKINOS_INPUT_WIRE_CREATE = 1,
    SKINOS_INPUT_WIRE_REPORT = 2,
    SKINOS_INPUT_WIRE_DESTROY = 3,
    SKINOS_INPUT_WIRE_HEADER_BYTES = 32,
    SKINOS_INPUT_WIRE_PAYLOAD_BYTES = 256,
    SKINOS_INPUT_WIRE_FRAME_BYTES = 288,
    SKINOS_INPUT_WIRE_SLOTS = 16,
};

// Custom own-backend observation API, not a Valve/Steamworks function. A relay
// must poll loss_generation: any increase requires disconnecting its Wine-only
// devices and failing the run, so a dropped release cannot leave a held button.
// Datagrams/this counter are not device-creation or input-delivery ACKs.
typedef struct SkinosInputOutputStats {
    uint32_t version;
    uint32_t bytes;
    uint64_t queued_frames;
    uint64_t failed_frames;
    uint64_t loss_generation;
    uint64_t rejected_active_creates;
    uint32_t active_slot_mask;
    uint32_t reserved;
} SkinosInputOutputStats;
enum {
    SKINOS_INPUT_WORD_MAGIC = 0,
    SKINOS_INPUT_WORD_VERSION = 1,
    SKINOS_INPUT_WORD_KIND = 2,
    SKINOS_INPUT_WORD_SLOT = 3,
    SKINOS_INPUT_WORD_PAYLOAD_LENGTH = 4,
    SKINOS_INPUT_WORD_VENDOR = 5,
    SKINOS_INPUT_WORD_PRODUCT = 6,
    SKINOS_INPUT_WORD_DEVICE_VERSION = 7,
};

static inline uint32_t skinos_input_wire_u32(const uint8_t *bytes) {
    return (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8)
        | ((uint32_t)bytes[2] << 16) | ((uint32_t)bytes[3] << 24);
}

// Receiver lifecycle contract:
// * accept only this owned connected endpoint, exact frame/version, slot<16,
//   and descriptor/report format; reject validation failures;
// * initialize a newly created Wine-only device to neutral (buttons/triggers/
//   axes zero), matching Valve's zeroed state on Create; do NOT invent a Send;
// * require Create before reports, apply reports byte-for-byte, and remove on
//   Destroy or verified owner shutdown;
// * acknowledge actual Wine device creation/routing separately. Sender's true
//   only confirms a datagram was queued. No frame proves an OS HID device exists.
#endif
