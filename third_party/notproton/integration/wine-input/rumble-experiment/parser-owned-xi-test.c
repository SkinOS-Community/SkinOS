#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "owned-xi.h"
static unsigned wide(const char *text, uint16_t out[256]) {
    unsigned n = 0;
    do { out[n] = (unsigned char)text[n]; } while (text[n++]);
    return n;
}
int main(void) {
    const char *xi = "\\\\?\\HID#VID_045E&PID_028E&XI_00#SkinOS-Wine-Output-00-00000001#{6c53d5fd-6480-440f-b618-476750c5e1a6}";
    const char *ig = "\\\\?\\HID#VID_045E&PID_028E&IG_15#SkinOS-Wine-Output-15-FFFFFFFF#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    uint16_t text[256]; SkinosOwnedHidIdentity id = {0};
    unsigned length = wide(xi, text), checks = 0;
    assert(skinos_owned_xi_path(text, length, &id) && id.slot == 0 && id.generation == 1); ++checks;
    assert(!skinos_owned_ig_generation(text, length, &id)); ++checks;
    for (unsigned n = 0; n < length; ++n) { assert(!skinos_owned_xi_path(text, n, &id)); ++checks; }
    for (unsigned i = 0; i < length - 1; ++i) {
        uint16_t old = text[i]; text[i] = '!';
        assert(!skinos_owned_xi_path(text, length, &id)); ++checks; text[i] = old;
    }
    text[length - 1] = '!'; text[length] = 0;
    assert(!skinos_owned_xi_path(text, length + 1, &id)); ++checks;
    length = wide(ig, text);
    assert(skinos_owned_ig_generation(text, length, &id) && id.slot == 15 && id.generation == 0xffffffff); ++checks;
    assert(!skinos_owned_xi_path(text, length, &id)); ++checks;
    const char *bad[] = {
        "\\\\?\\HID#VID_045E&PID_028E&XI_00#SkinOS-Wine-Output-00-00000000#{6c53d5fd-6480-440f-b618-476750c5e1a6}",
        "\\\\?\\HID#VID_045E&PID_028E&XI_00#SkinOS-Wine-Output-01-00000001#{6c53d5fd-6480-440f-b618-476750c5e1a6}",
        "\\\\?\\HID#VID_045E&PID_028E&XI_16#SkinOS-Wine-Output-16-00000001#{6c53d5fd-6480-440f-b618-476750c5e1a6}",
    };
    for (unsigned i = 0; i < sizeof(bad) / sizeof(*bad); ++i) {
        length = wide(bad[i], text); assert(!skinos_owned_xi_path(text, length, &id)); ++checks;
    }
    printf("PASS: bounded owned XI/IG identity: %u checks\n", checks);
    return 0;
}
