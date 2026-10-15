#ifndef SKINOS_STEAM_INPUT_OUTPUT_H
#define SKINOS_STEAM_INPUT_OUTPUT_H

// ABI at the original Valve *output* module boundary, not a Steamworks API.
// Audit: engine SHA256 035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840;
// original universal libsteaminput SHA256
// 7b09276160dfc42d1b8bb950a70b2f6a6802fa322728fe958f5ae4761b6eb585.
// ARM64 exported Create 0x3858, Send 0x3b74, Destroy 0x3c08.
// Original REA evidence: ev_fa0b2392c06eddab98b93ff87bb2311e1fd116b9a41f767265af3f4ae9fa3375.
// This header supplies types/decoding only. It does not load/interpose any
// library, create HID devices, authenticate, or manufacture input/handles.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// Length travels in the full 64-bit register and is passed to CFDataCreate /
// IOHIDUserDeviceHandleReportWithTimeStamp (CFIndex). Only nonnegative bounded
// lengths are valid. IDs/version are read through 32-bit values by Create.
typedef bool (*SkinosValveInputCreateFn)(
    const char *product_key, const char *manufacturer, const char *serial,
    uint32_t vendor_id, uint32_t product_id, uint32_t device_version,
    const uint8_t *report_descriptor, intptr_t descriptor_bytes);
typedef bool (*SkinosValveInputSendFn)(
    const char *product_key, const uint8_t *report, intptr_t report_bytes);
typedef bool (*SkinosValveInputDestroyFn)(const char *product_key);
typedef bool (*SkinosValveInputAvailableFn)(void);

// product_key is the CFDictionary lookup key, e.g. "GamePad-1" for the engine's
// slot 0. It is NOT a Steam Input handle. Create reports true only for nonnull
// IOHIDUserDevice creation. Send reports actual HandleReport success. Destroy's
// true means the module's dictionary existed, even if this key was absent.
// Available unconditionally returns true in the inspected library; it does not
// validate OS entitlements, device publication, or Wine delivery.

#define SKINOS_VALVE_GAMEPAD_REPORT_BYTES 20u
#define SKINOS_VALVE_GAMEPAD_DESCRIPTOR_BYTES 188u
#define SKINOS_VALVE_GAMEPAD_DESCRIPTOR_SHA256 "086699843817c3d3554b4de8d239d809407b328eafa4a82aae93284f669524a7"
#define SKINOS_VALVE_GAMEPAD_VENDOR_ID 0x045eu
#define SKINOS_VALVE_GAMEPAD_PRODUCT_ID 0x028eu

// Exact mapped gamepad bytes from engine wrapper 0xb124ec. The descriptor at
// 0x17e8ba8 declares 112 INPUT bits (14 bytes), no REPORT_ID, OUTPUT or FEATURE.
// The engine sends 20 bytes, preserving six unused trailing bytes. Its setters
// at 0x7021a8 / 0x702488 update this state and send only when bytes have changed.
// The first two bytes are constant padding, NOT an ID/length header.
typedef struct SkinosValveMappedGamepadReport {
    uint8_t reserved_prefix[2];
    uint8_t buttons_le[2];
    uint8_t left_trigger;
    uint8_t right_trigger;
    uint8_t left_x_le[2];
    uint8_t left_y_le[2];
    uint8_t right_x_le[2];
    uint8_t right_y_le[2];
    uint8_t unused_tail[6];
} SkinosValveMappedGamepadReport;

// Button bits match the traditional XInput layout, including bit 10 (Guide).
// Bit 11 is HID constant padding. Do not reinterpret generic SDL button order.
enum {
    SKINOS_PAD_UP = 0x0001, SKINOS_PAD_DOWN = 0x0002,
    SKINOS_PAD_LEFT = 0x0004, SKINOS_PAD_RIGHT = 0x0008,
    SKINOS_PAD_START = 0x0010, SKINOS_PAD_BACK = 0x0020,
    SKINOS_PAD_LEFT_STICK = 0x0040, SKINOS_PAD_RIGHT_STICK = 0x0080,
    SKINOS_PAD_LEFT_SHOULDER = 0x0100, SKINOS_PAD_RIGHT_SHOULDER = 0x0200,
    SKINOS_PAD_GUIDE = 0x0400,
    SKINOS_PAD_A = 0x1000, SKINOS_PAD_B = 0x2000,
    SKINOS_PAD_X = 0x4000, SKINOS_PAD_Y = 0x8000
};

static inline uint16_t skinos_input_u16le(const uint8_t bytes[2]) {
    return (uint16_t)((uint16_t)bytes[0] | ((uint16_t)bytes[1] << 8));
}
static inline int16_t skinos_input_i16le(const uint8_t bytes[2]) {
    uint16_t value = skinos_input_u16le(bytes);
    return (int16_t)((int32_t)value - ((value & 0x8000u) ? 65536 : 0));
}

// There is NO original rumble callback in this export ABI. Create registers
// GetReport/SetReport blocks at 0x3c80/0x3c88 which both just return IOReturn(0),
// and ignore the report arguments. Any future reverse rumble path must call a
// separately verified genuine host API; it cannot be inferred from these calls.

#if defined(__cplusplus)
static_assert(sizeof(intptr_t) == 8, "pinned output ABI requires a 64-bit process");
static_assert(sizeof(SkinosValveMappedGamepadReport) == 20, "mapped report size");
static_assert(offsetof(SkinosValveMappedGamepadReport, left_trigger) == 4, "trigger offset");
static_assert(offsetof(SkinosValveMappedGamepadReport, left_x_le) == 6, "axis offset");
static_assert(offsetof(SkinosValveMappedGamepadReport, unused_tail) == 14, "tail offset");
#else
_Static_assert(sizeof(intptr_t) == 8, "pinned output ABI requires a 64-bit process");
_Static_assert(sizeof(SkinosValveMappedGamepadReport) == 20, "mapped report size");
#endif

#endif
