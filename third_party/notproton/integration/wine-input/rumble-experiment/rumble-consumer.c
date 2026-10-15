/* Isolated public-API Wine rumble probe. API success is NOT delivery proof.
 * Parent joins this transcript with the owned feeder's actual dequeue trace. */
#define mainCRTStartup original_transport_entry
#include "consumer.c"
#undef mainCRTStartup
#include "setupapi.h"
#include "ddk/hidsdi.h"
#include "hidusage.h"
#include "owned-xi.h"
#include "haptics.h"

typedef DWORD (WINAPI *SetState)(DWORD, XINPUT_VIBRATION *);
static const GUID xi_interface = {0x6c53d5fd,0x6480,0x440f,{0xb6,0x18,0x47,0x67,0x50,0xc5,0xe1,0xa6}};
typedef struct RumbleInspection {
    HANDLE device;
    PHIDP_PREPARSED_DATA preparsed;
    Input *input;
    WCHAR path[MAX_PATH], ig_path[MAX_PATH];
    SkinosOwnedHidIdentity xi, ig;
    HIDP_CAPS caps;
    DWORD xi_total, xi_owned, di_total, di_owned, connected, error, xinput_result;
    NTSTATUS hidp_status;
    HRESULT di_result;
    BYTE feature[SKINOS_HAPTICS_FEATURE_BYTES];
    BOOL feature_verified, input_ids_verified, haptics_verified;
    const char *stage;
} RumbleInspection;

static void close_inspection(RumbleInspection *r) {
    if (r->preparsed) HidD_FreePreparsedData(r->preparsed);
    if (r->device && r->device != INVALID_HANDLE_VALUE) CloseHandle(r->device);
    r->preparsed = NULL; r->device = INVALID_HANDLE_VALUE;
}
static BOOL inspection_error(RumbleInspection *r, const char *stage, DWORD error) {
    r->stage = stage; r->error = error ? error : ERROR_INVALID_DATA; return FALSE;
}
static BOOL public_input_ids(RumbleInspection *r) {
    /* The pinned input descriptor has fewer than sixteen of either cap type.
     * Keep this bounded frame below ARM64's external __chkstk threshold. */
    HIDP_VALUE_CAPS values[16]; HIDP_BUTTON_CAPS buttons[16];
    USHORT vn = 16, bn = 16;
    r->hidp_status = HidP_GetValueCaps(HidP_Input, values, &vn, r->preparsed);
    if (r->hidp_status != HIDP_STATUS_SUCCESS || !vn) return FALSE;
    r->hidp_status = HidP_GetButtonCaps(HidP_Input, buttons, &bn, r->preparsed);
    if (r->hidp_status != HIDP_STATUS_SUCCESS || !bn) return FALSE;
    for (USHORT i = 0; i < vn; ++i) if (values[i].ReportID) return FALSE;
    for (USHORT i = 0; i < bn; ++i) if (buttons[i].ReportID) return FALSE;
    return TRUE;
}
static BOOL public_haptics_caps(RumbleInspection *r) {
    HIDP_LINK_COLLECTION_NODE nodes[16]; ULONG count = 16;
    unsigned rumble = 0, buzz = 0; USHORT rumble_collection = 0, buzz_collection = 0;
    r->hidp_status = HidP_GetLinkCollectionNodes(nodes, &count, r->preparsed);
    if (r->hidp_status != HIDP_STATUS_SUCCESS || !count || count > 16) return FALSE;
    for (ULONG i = 0; i < count; ++i) {
        HIDP_VALUE_CAPS feature, intensity; USHORT n = 1; ULONG value = 0;
        if (nodes[i].LinkUsagePage != HID_USAGE_PAGE_HAPTICS ||
            nodes[i].LinkUsage != HID_USAGE_HAPTICS_WAVEFORM_LIST) continue;
        ULONG parent = nodes[i].Parent;
        if (parent >= count || nodes[parent].LinkUsagePage != HID_USAGE_PAGE_HAPTICS ||
            nodes[parent].LinkUsage != HID_USAGE_HAPTICS_SIMPLE_CONTROLLER || nodes[parent].Parent != 0) return FALSE;
        r->hidp_status = HidP_GetSpecificValueCaps(HidP_Feature, HID_USAGE_PAGE_ORDINAL,
                                                  (USHORT)i, 3, &feature, &n, r->preparsed);
        if (r->hidp_status != HIDP_STATUS_SUCCESS || n != 1 || feature.ReportID != 1 ||
            feature.BitSize != 16 || feature.LogicalMin != 0 || feature.LogicalMax != 65535) return FALSE;
        r->hidp_status = HidP_GetUsageValue(HidP_Feature, HID_USAGE_PAGE_ORDINAL, (USHORT)i, 3,
                                           &value, r->preparsed, (char *)r->feature, sizeof(r->feature));
        if (r->hidp_status != HIDP_STATUS_SUCCESS) return FALSE;
        n = 1;
        r->hidp_status = HidP_GetSpecificValueCaps(HidP_Output, HID_USAGE_PAGE_HAPTICS,
            (USHORT)parent, HID_USAGE_HAPTICS_INTENSITY, &intensity, &n, r->preparsed);
        if (r->hidp_status != HIDP_STATUS_SUCCESS || n != 1 || intensity.ReportID != 1 ||
            intensity.BitSize != 16 || intensity.LogicalMin != 0 || intensity.LogicalMax != 65535) return FALSE;
        if (value == HID_USAGE_HAPTICS_WAVEFORM_RUMBLE) { ++rumble; rumble_collection = (USHORT)parent; }
        else if (value == HID_USAGE_HAPTICS_WAVEFORM_BUZZ) { ++buzz; buzz_collection = (USHORT)parent; }
        else return FALSE;
    }
    return rumble == 1 && buzz == 1 && rumble_collection != buzz_collection;
}
static BOOL CALLBACK enumerate_rumble_di(const DeviceInstance *instance, void *context) {
    RumbleInspection *r = context; void *device = NULL;
    ++r->di_total;
    HRESULT result = r->input->v->CreateDevice(r->input, &instance->guidInstance, &device, NULL);
    if (result < 0 || !device) return TRUE;
    const void **v = *(const void ***)device;
    HRESULT (WINAPI *get_property)(void *, const GUID *, void *) = (void *)v[5];
    ULONG (WINAPI *release)(void *) = (void *)v[2];
    DiGuidPath value; memset(&value, 0, sizeof(value)); value.size = sizeof(value); value.header_size = 16;
    result = get_property(device, (const GUID *)12, &value); release(device);
    SkinosOwnedHidIdentity id;
    if (result >= 0 && skinos_owned_ig_generation((const uint16_t *)value.path, MAX_PATH, &id)) {
        ++r->di_owned; r->ig = id; memcpy(r->ig_path, value.path, sizeof(r->ig_path));
    }
    return TRUE;
}
static BOOL inspect_owned(Input *input, GetState get, const SkinosOwnedHidIdentity *bound, RumbleInspection *r) {
    memset(r, 0, sizeof(*r)); r->device = INVALID_HANDLE_VALUE; r->input = input;
    r->xinput_result = ERROR_DEVICE_NOT_CONNECTED;
    HDEVINFO devices = SetupDiGetClassDevsW(&xi_interface, NULL, NULL, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (devices == INVALID_HANDLE_VALUE) return inspection_error(r, "xi_enumeration", GetLastError());
    BOOL complete = FALSE;
    for (DWORD index = 0; index < 64; ++index) {
        SP_DEVICE_INTERFACE_DATA iface; memset(&iface, 0, sizeof(iface)); iface.cbSize = sizeof(iface);
        if (!SetupDiEnumDeviceInterfaces(devices, NULL, &xi_interface, index, &iface)) {
            DWORD error = GetLastError(); complete = error == ERROR_NO_MORE_ITEMS;
            if (!complete) inspection_error(r, "xi_enumeration", error);
            break;
        }
        ++r->xi_total;
        union { ULONGLONG align; BYTE bytes[sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W) + MAX_PATH * sizeof(WCHAR)]; } storage;
        memset(&storage, 0, sizeof(storage));
        SP_DEVICE_INTERFACE_DETAIL_DATA_W *detail = (void *)storage.bytes;
        detail->cbSize = sizeof(*detail);
        if (!SetupDiGetDeviceInterfaceDetailW(devices, &iface, detail, sizeof(storage), NULL, NULL)) {
            inspection_error(r, "xi_detail", GetLastError()); break;
        }
        SkinosOwnedHidIdentity id;
        if (skinos_owned_xi_path((const uint16_t *)detail->DevicePath, MAX_PATH, &id)) {
            ++r->xi_owned; r->xi = id; memcpy(r->path, detail->DevicePath, sizeof(r->path));
        }
    }
    SetupDiDestroyDeviceInfoList(devices);
    if (!complete || r->xi_total != 1 || r->xi_owned != 1 || r->xi.slot != 0)
        return inspection_error(r, r->stage ? r->stage : "single_owned_xi", r->error);
    if (bound->generation && (bound->slot != r->xi.slot || bound->generation != r->xi.generation))
        return inspection_error(r, "pdo_generation_changed", ERROR_DEVICE_NOT_CONNECTED);
    r->device = CreateFileW(r->path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
                            NULL, OPEN_EXISTING, 0, NULL);
    if (r->device == INVALID_HANDLE_VALUE) return inspection_error(r, "xi_open", GetLastError());
    HIDD_ATTRIBUTES attrs; memset(&attrs, 0, sizeof(attrs)); attrs.Size = sizeof(attrs);
    if (!HidD_GetAttributes(r->device, &attrs) || attrs.VendorID != 0x045e || attrs.ProductID != 0x028e)
        return inspection_error(r, "xi_attributes", GetLastError());
    if (!HidD_GetPreparsedData(r->device, &r->preparsed)) return inspection_error(r, "preparsed_data", GetLastError());
    r->hidp_status = HidP_GetCaps(r->preparsed, &r->caps);
    if (r->hidp_status != HIDP_STATUS_SUCCESS || r->caps.UsagePage != HID_USAGE_PAGE_GENERIC ||
        r->caps.Usage != HID_USAGE_GENERIC_GAMEPAD || r->caps.InputReportByteLength != 15 ||
        r->caps.OutputReportByteLength != 5 || r->caps.FeatureReportByteLength != 17)
        return inspection_error(r, "hid_caps", ERROR_INVALID_DATA);
    if (!(r->input_ids_verified = public_input_ids(r))) return inspection_error(r, "input_report_id", ERROR_INVALID_DATA);
    r->feature[0] = SKINOS_HAPTICS_REPORT_ID;
    if (!HidD_GetFeature(r->device, r->feature, sizeof(r->feature))) return inspection_error(r, "get_feature", GetLastError());
    if (!(r->feature_verified = !memcmp(r->feature, skinos_haptics_feature, sizeof(r->feature))))
        return inspection_error(r, "feature_content", ERROR_INVALID_DATA);
    if (!(r->haptics_verified = public_haptics_caps(r))) return inspection_error(r, "haptics_caps", ERROR_INVALID_DATA);
    r->di_result = input->v->EnumDevices(input, 4, enumerate_rumble_di, r, 1);
    if (r->di_result < 0 || r->di_total != 1 || r->di_owned != 1 ||
        r->ig.slot != r->xi.slot || r->ig.generation != r->xi.generation)
        return inspection_error(r, "matching_owned_ig", ERROR_INVALID_DATA);
    for (DWORD slot = 0; slot < 4; ++slot) {
        XINPUT_STATE state; DWORD result = get(slot, &state);
        if (!slot) r->xinput_result = result;
        if (!result) { ++r->connected; if (slot) return inspection_error(r, "foreign_xinput_index", ERROR_INVALID_DATA); }
        else if (result != ERROR_DEVICE_NOT_CONNECTED) return inspection_error(r, "xinput_get_state", result);
    }
    if (r->connected != 1 || r->xinput_result) return inspection_error(r, "single_xinput_index_zero", r->xinput_result);
    return TRUE;
}
static void json_path(const WCHAR *path) {
    if (!path[0]) { out("null"); return; }
    out("\"");
    for (unsigned i = 0; i < MAX_PATH && path[i]; ++i) {
        char text[2] = {(char)path[i], 0};
        if (path[i] == '\\' || path[i] == '"') out("\\");
        out(text); /* The exact owned path matcher permits ASCII only. */
    }
    out("\"");
}
static void emit_result(const SkinosInputFrame *f, const RumbleInspection *r, BOOL ok, BOOL called, DWORD result) {
    out("{\"event\":\"wine_rumble_result\",\"ok\":"); out(ok ? "true" : "false");
    out(",\"kind\":"); num(f->kind); out(",\"slot\":"); num(f->slot);
    out(",\"owned_slot\":"); num(r->xi.slot); out(",\"generation\":"); num(r->xi.generation);
    if (f->kind == 5) { out(",\"left\":"); num(u16(f->payload)); out(",\"right\":"); num(u16(f->payload + 2)); }
    out(",\"xinput_result\":"); num(result); out(",\"xinput_set_called\":"); out(called ? "true" : "false");
    out(",\"input_bytes\":"); num(r->caps.InputReportByteLength);
    out(",\"output_bytes\":"); num(r->caps.OutputReportByteLength);
    out(",\"feature_bytes\":"); num(r->caps.FeatureReportByteLength);
    out(",\"feature_verified\":"); out(r->feature_verified ? "true" : "false");
    out(",\"input_id_zero_verified\":"); out(r->input_ids_verified ? "true" : "false");
    out(",\"haptics_caps_verified\":"); out(r->haptics_verified ? "true" : "false");
    out(",\"xi_interfaces\":"); num(r->xi_total); out(",\"owned_xi_interfaces\":"); num(r->xi_owned);
    out(",\"dinput_devices\":"); num(r->di_total); out(",\"owned_dinput_devices\":"); num(r->di_owned);
    out(",\"xinput_connected_count\":"); num(r->connected);
    out(",\"device_path\":"); json_path(r->path); out(",\"ig_path\":"); json_path(r->ig_path);
    out(",\"inspection_error\":"); num(r->error); out(",\"hidp_status\":"); num((DWORD)r->hidp_status);
    out(",\"inspection_stage\":"); if (r->stage) { out("\""); out(r->stage); out("\""); } else out("null");
    out(",\"transport_only\":true,\"delivery_verified\":false,\"physical_device_verified\":false}\n");
}
void mainCRTStartup(void) {
    HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
    if (!ntdll || !GetProcAddress(ntdll, "wine_get_version")) { out("{\"event\":\"wine_required\",\"ok\":false}\n"); ExitProcess(1); }
    HMODULE x = LoadLibraryW(L"xinput1_4.dll"), d = LoadLibraryW(L"dinput8.dll");
    GetState get = x ? (GetState)GetProcAddress(x, "XInputGetState") : NULL;
    SetState set = x ? (SetState)GetProcAddress(x, "XInputSetState") : NULL;
    CreateInput create = d ? (CreateInput)GetProcAddress(d, "DirectInput8Create") : NULL;
    Input *input = NULL; HRESULT hr = create ? create(GetModuleHandleW(NULL), 0x0800, &iid_input8w, (void **)&input, NULL) : E_FAIL;
    if (!get || !set || hr < 0 || !input) { out("{\"event\":\"wine_rumble_consumer_init\",\"ok\":false}\n"); ExitProcess(1); }
    out("{\"event\":\"wine_rumble_consumer_ready\",\"transport_only\":true,\"delivery_verified\":false}\n");
    DWORD failure = 0; BOOL stop_pending = FALSE; SkinosOwnedHidIdentity bound = {0};
    for (;;) {
        SkinosInputFrame f; DWORD have = 0;
        while (have < sizeof(f)) {
            DWORD n = 0; if (!ReadFile(GetStdHandle(STD_INPUT_HANDLE), (BYTE *)&f + have, sizeof(f) - have, &n, NULL) || !n) break;
            have += n;
        }
        if (!have) break;
        if (have != sizeof(f) || f.magic != SKINOS_INPUT_MAGIC || f.version != 1 || f.slot != 0 ||
            !((f.kind == 5 && f.payload_len == 4) || (f.kind == 6 && f.payload_len == 0))) {
            out("{\"event\":\"wine_rumble_invalid_frame\",\"ok\":false,\"xinput_set_called\":false}\n"); failure = 1; break;
        }
        RumbleInspection r; BOOL ok, called = FALSE; DWORD began = GetTickCount();
        do {
            ok = inspect_owned(input, get, &bound, &r);
            if (ok || bound.generation || GetTickCount() - began >= 5000) break;
            close_inspection(&r); Sleep(25);
        } while (TRUE);
        DWORD result = ok ? r.xinput_result : r.error;
        if (ok && !bound.generation) bound = r.xi;
        if (ok && f.kind == 5) {
            XINPUT_VIBRATION motors = {(WORD)u16(f.payload), (WORD)u16(f.payload + 2)};
            stop_pending = TRUE; called = TRUE; result = set(0, &motors); ok = result == ERROR_SUCCESS;
            if (ok) stop_pending = motors.wLeftMotorSpeed || motors.wRightMotorSpeed;
        }
        emit_result(&f, &r, ok, called, result); close_inspection(&r);
        if (!ok) failure = 1;
    }
    if (stop_pending) {
        RumbleInspection r; BOOL safe = inspect_owned(input, get, &bound, &r); DWORD result = r.error;
        if (safe) { XINPUT_VIBRATION zero = {0}; result = set(0, &zero); }
        out("{\"event\":\"wine_rumble_consumer_cleanup\",\"xinput_set_called\":"); out(safe ? "true" : "false");
        out(",\"xinput_result\":"); num(result); out(",\"generation\":"); num(bound.generation);
        out(",\"delivery_verified\":false,\"physical_device_verified\":false}\n");
        if (!safe || result) failure = 1;
        close_inspection(&r);
    }
    input->v->Release(input); ExitProcess(failure);
}
