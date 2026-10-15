#ifndef SKINOS_OWNED_RUMBLE_XI_PATH_H
#define SKINOS_OWNED_RUMBLE_XI_PATH_H
#include <stdint.h>
typedef struct SkinosOwnedHidIdentity { unsigned slot; uint32_t generation; } SkinosOwnedHidIdentity;
static inline unsigned skinos_xi_lower(unsigned c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }
static inline int skinos_xi_literal(const uint16_t *p, unsigned size, unsigned *at, const char *s) {
    while (*s) { if (*at >= size || skinos_xi_lower(p[*at]) != (unsigned char)*s++) return 0; ++*at; }
    return 1;
}
/* Bounded full-path matching: no product-name/substring ownership claim. */
static inline int skinos_owned_rumble_path(const uint16_t *p, unsigned size, int xi, SkinosOwnedHidIdentity *out) {
    unsigned at = 0, slot, serial_slot, generation = 0;
    if (!p || !out || !skinos_xi_literal(p, size, &at, "\\\\?\\hid#vid_045e&pid_028e&")) return 0;
    if (!skinos_xi_literal(p, size, &at, xi ? "xi_" : "ig_")) return 0;
    if (at + 2 > size || p[at] < '0' || p[at] > '9' || p[at + 1] < '0' || p[at + 1] > '9') return 0;
    slot = (p[at] - '0') * 10 + p[at + 1] - '0'; at += 2;
    if (slot >= 16 || !skinos_xi_literal(p, size, &at, "#skinos-wine-output-")) return 0;
    if (at + 2 > size || p[at] < '0' || p[at] > '9' || p[at + 1] < '0' || p[at + 1] > '9') return 0;
    serial_slot = (p[at] - '0') * 10 + p[at + 1] - '0'; at += 2;
    if (serial_slot != slot || !skinos_xi_literal(p, size, &at, "-")) return 0;
    for (unsigned i = 0; i < 8; ++i) {
        unsigned c, value;
        if (at >= size) return 0;
        c = skinos_xi_lower(p[at++]);
        if (c >= '0' && c <= '9') value = c - '0';
        else if (c >= 'a' && c <= 'f') value = c - 'a' + 10;
        else return 0;
        generation = (generation << 4) | value;
    }
    if (!generation || !skinos_xi_literal(p, size, &at, xi ?
        "#{6c53d5fd-6480-440f-b618-476750c5e1a6}" : "#{4d1e55b2-f16f-11cf-88cb-001111000030}") ||
        at >= size || p[at]) return 0;
    out->slot = slot; out->generation = generation; return 1;
}
static inline int skinos_owned_xi_path(const uint16_t *p, unsigned size, SkinosOwnedHidIdentity *out) {
    return skinos_owned_rumble_path(p, size, 1, out);
}
static inline int skinos_owned_ig_generation(const uint16_t *p, unsigned size, SkinosOwnedHidIdentity *out) {
    return skinos_owned_rumble_path(p, size, 0, out);
}
#endif
