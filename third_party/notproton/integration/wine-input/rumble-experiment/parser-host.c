/* Offline descriptor QA using the unchanged, pinned Wine parser and extracted
 * stock XInput waveform selector. Only pool allocation and HID Feature I/O are
 * host substitutes. This does not establish live Wine or motor delivery. */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "windef.h"
#include "winbase.h"
#include "winternl.h"
#include "winioctl.h"
#include "ddk/wdm.h"
#include "ddk/hidpddi.h"
#include "ddk/hidsdi.h"
#include "wine/hid.h"
#include "wine/debug.h"

#define skinos_wine_descriptor baseline_descriptor
#define skinos_translate_report baseline_translate_report
#include "../adapter.h"
#undef skinos_wine_descriptor
#undef skinos_translate_report
#undef SKINOS_WINE_INPUT_ADAPTER_H
#include "adapter.h"

PVOID WINAPI ExAllocatePool(POOL_TYPE pool, SIZE_T size) { (void)pool; return malloc(size); }
void WINAPI ExFreePool(PVOID ptr) { free(ptr); }

struct xinput_controller {
    struct {
        HIDP_CAPS caps;
        char *feature_report_buf;
        BYTE haptics_report;
        HIDP_VALUE_CAPS haptics_rumble_caps, haptics_buzz_caps;
    } hid;
};
static unsigned feature_reads;
BOOLEAN WINAPI HidD_GetFeature(HANDLE device, PVOID report, ULONG length) {
    assert(device == (HANDLE)(uintptr_t)0xfeed);
    assert(length == sizeof(skinos_haptics_feature));
    assert(((unsigned char *)report)[0] == 1);
    memcpy(report, skinos_haptics_feature, length);
    ++feature_reads;
    return TRUE;
}

/* Generated verbatim from dlls/xinput1_3/main.c by the runner. */
#include "wine-xinput-waveform.c"

static void success(NTSTATUS status) { assert(status == HIDP_STATUS_SUCCESS); }
static void check_input_caps(PHIDP_PREPARSED_DATA baseline, PHIDP_PREPARSED_DATA rumble) {
    struct hid_preparsed_data *a = (void *)baseline, *b = (void *)rumble;
    assert(a->input_caps_count == b->input_caps_count);
    assert(a->input_caps_end == b->input_caps_end);
    assert(!memcmp(HID_INPUT_VALUE_CAPS(a), HID_INPUT_VALUE_CAPS(b),
                   a->input_caps_count * sizeof(struct hid_value_caps)));
    HIDP_VALUE_CAPS av[32] = {0}, bv[32] = {0};
    HIDP_BUTTON_CAPS ab[32] = {0}, bb[32] = {0};
    USHORT an = 32, bn = 32;
    success(HidP_GetValueCaps(HidP_Input, av, &an, baseline));
    success(HidP_GetValueCaps(HidP_Input, bv, &bn, rumble));
    assert(an == bn && !memcmp(av, bv, an * sizeof(*av)));
    an = bn = 32;
    success(HidP_GetButtonCaps(HidP_Input, ab, &an, baseline));
    success(HidP_GetButtonCaps(HidP_Input, bb, &bn, rumble));
    assert(an == bn && !memcmp(ab, bb, an * sizeof(*ab)));
    for (unsigned i = 0; i < b->input_caps_count; ++i) {
        const struct hid_value_caps *c = HID_INPUT_VALUE_CAPS(b) + i;
        assert(c->report_id == 0);
        printf("input page=%04x usage=%04x..%04x byte=%u bit=%u width=%u count=%u id=%u\n",
               c->usage_page, c->usage_min, c->usage_max, c->start_byte, c->start_bit,
               c->bit_size, c->report_count, c->report_id);
    }
}

int main(void) {
    _Static_assert(sizeof(ULONG) == 4 && sizeof(USHORT) == 2 && sizeof(WCHAR) == 2, "Wine host ABI");
    assert(!memcmp(baseline_descriptor, skinos_wine_descriptor, sizeof(baseline_descriptor) - 1));
    assert(skinos_wine_descriptor[sizeof(baseline_descriptor) - 1] == 0xa4); /* PUSH */
    assert(skinos_wine_descriptor[sizeof(skinos_wine_descriptor) - 2] == 0xb4); /* POP */
    assert(skinos_wine_descriptor[sizeof(skinos_wine_descriptor) - 1] == 0xc0); /* same TLC */
    HIDP_DEVICE_DESC base = {0}, desc = {0};
    success(HidP_GetCollectionDescription((BYTE *)baseline_descriptor,
                sizeof(baseline_descriptor), PagedPool, &base));
    success(HidP_GetCollectionDescription((BYTE *)skinos_wine_descriptor,
                sizeof(skinos_wine_descriptor), PagedPool, &desc));
    assert(base.CollectionDescLength == 1 && desc.CollectionDescLength == 1);
    PHIDP_PREPARSED_DATA prep = desc.CollectionDesc[0].PreparsedData;
    HIDP_CAPS caps = {0};
    success(HidP_GetCaps(prep, &caps));
    assert(caps.UsagePage == HID_USAGE_PAGE_GENERIC && caps.Usage == HID_USAGE_GENERIC_GAMEPAD);
    assert(caps.InputReportByteLength == 15);
    assert(caps.OutputReportByteLength == 5);
    assert(caps.FeatureReportByteLength == 17);
    assert(desc.ReportIDsLength == 2);
    assert(desc.ReportIDs[0].ReportID == 0 && desc.ReportIDs[0].InputLength == 15);
    assert(!desc.ReportIDs[0].OutputLength && !desc.ReportIDs[0].FeatureLength);
    assert(desc.ReportIDs[1].ReportID == 1 && !desc.ReportIDs[1].InputLength);
    assert(desc.ReportIDs[1].OutputLength == 5 && desc.ReportIDs[1].FeatureLength == 17);
    check_input_caps(base.CollectionDesc[0].PreparsedData, prep);

    HIDP_LINK_COLLECTION_NODE nodes[32] = {0};
    ULONG nn = 32;
    success(HidP_GetLinkCollectionNodes(nodes, &nn, prep));
    char feature[17] = {0};
    struct xinput_controller controller = {.hid = {.caps = caps, .feature_report_buf = feature}};
    unsigned waveforms = 0;
    for (ULONG i = 0; i < nn; ++i) {
        if (nodes[i].LinkUsagePage != HID_USAGE_PAGE_HAPTICS ||
            nodes[i].LinkUsage != HID_USAGE_HAPTICS_WAVEFORM_LIST) continue;
        HIDP_VALUE_CAPS values[8] = {0};
        USHORT count = 8;
        success(HidP_GetSpecificValueCaps(HidP_Feature, HID_USAGE_PAGE_ORDINAL, i, 0, values, &count, prep));
        for (USHORT j = 0; j < count; ++j) {
            assert(values[j].ReportID == 1);
            assert(values[j].LogicalMin == 0 && values[j].LogicalMax == 65535);
            check_waveform_caps(&controller, (HANDLE)(uintptr_t)0xfeed, prep, nodes, &values[j]);
            ++waveforms;
        }
    }
    assert(waveforms == 2 && feature_reads == 2);
    assert(controller.hid.haptics_report == 1);
    assert(controller.hid.haptics_rumble_caps.UsagePage == HID_USAGE_PAGE_HAPTICS);
    assert(controller.hid.haptics_buzz_caps.UsagePage == HID_USAGE_PAGE_HAPTICS);
    USHORT left = controller.hid.haptics_rumble_caps.LinkCollection;
    USHORT right = controller.hid.haptics_buzz_caps.LinkCollection;
    assert(left != right && left > 0 && right > 0);
    struct hid_preparsed_data *internal = (void *)prep;
    for (unsigned i = 0; i < internal->feature_caps_count; ++i) {
        const struct hid_value_caps *c = HID_FEATURE_VALUE_CAPS(internal) + i;
        ULONG value = 0;
        assert(c->report_id == 1);
        success(HidP_GetUsageValue(HidP_Feature, c->usage_page, c->link_collection,
                    c->usage_min, &value, prep, (char *)skinos_haptics_feature, sizeof(skinos_haptics_feature)));
        if (c->usage_page == HID_USAGE_PAGE_HAPTICS)
            assert(c->usage_min == HID_USAGE_HAPTICS_WAVEFORM_CUTOFF_TIME && value == 1000);
        else {
            assert(c->usage_page == HID_USAGE_PAGE_ORDINAL && c->usage_min == 3);
            if (nodes[c->link_collection].LinkUsage == HID_USAGE_HAPTICS_DURATION_LIST) assert(value == 0);
            else assert(value == HID_USAGE_HAPTICS_WAVEFORM_RUMBLE || value == HID_USAGE_HAPTICS_WAVEFORM_BUZZ);
        }
        printf("feature page=%04x usage=%04x byte=%u bit=%u width=%u value=%u id=%u collection=%u\n",
               c->usage_page, c->usage_min, c->start_byte, c->start_bit, c->bit_size,
               (unsigned)value, c->report_id, c->link_collection);
    }
    static const unsigned values[] = {0, 1, 0x1234, 0x8000, 0xffff};
    unsigned cases = 0;
    for (unsigned i = 0; i < sizeof(values) / sizeof(*values); ++i)
        for (unsigned j = 0; j < sizeof(values) / sizeof(*values); ++j) {
            char report[5];
            success(HidP_InitializeReportForID(HidP_Output, 1, prep, report, sizeof(report)));
            success(HidP_SetUsageValue(HidP_Output, HID_USAGE_PAGE_HAPTICS, left,
                        HID_USAGE_HAPTICS_INTENSITY, values[i], prep, report, sizeof(report)));
            success(HidP_SetUsageValue(HidP_Output, HID_USAGE_PAGE_HAPTICS, right,
                        HID_USAGE_HAPTICS_INTENSITY, values[j], prep, report, sizeof(report)));
            unsigned char expected[5] = {1, values[i] & 255, values[i] >> 8, values[j] & 255, values[j] >> 8};
            assert(!memcmp(report, expected, sizeof(report)));
            ++cases;
        }
    printf("PASS: real Wine parser; caps=15/5/17; input caps identical; ID0/ID1 separated; "
           "stock XInput rumble collection=%u buzz collection=%u; output cases=%u\n", left, right, cases);
    HidP_FreeCollectionDescription(&base);
    HidP_FreeCollectionDescription(&desc);
    return 0;
}
