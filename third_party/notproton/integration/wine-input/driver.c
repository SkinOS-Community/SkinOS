/*
 * SkinOS Wine-only input bus.
 * Copyright 2026 SkinOS contributors
 *
 * PnP/HID queue design follows Wine dlls/dinput/tests/driver_bus.c:
 * Copyright 2022 Remi Bernon for CodeWeavers.
 *
 * This library is free software; you can redistribute it and/or modify it
 * under the terms of the GNU Lesser General Public License as published by
 * the Free Software Foundation; either version 2.1 of the License, or
 * (at your option) any later version. This library is distributed WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE. See the GNU Lesser General Public
 * License for more details.
 *
 * No host HID, generated controller state, or replacement Wine DLL is used.
 * A helper writes only genuine mapped producer frames to this owned device.
 * Requires a root-enumerated SkinOsInputBus device installed by SetupAPI.
 */
#include <stddef.h>
#include <stdarg.h>
#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "windef.h"
#include "winbase.h"
#include "winternl.h"
#include "winioctl.h"
#include "ddk/wdm.h"
#include "ddk/hidport.h"
#include "ddk/hidclass.h"
#include "descriptor.h"
#include "adapter.h"

#define SKINOS_IOCTL_FRAME CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_WRITE_ACCESS)
#define SKINOS_IOCTL_STATUS CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_WRITE_ACCESS)
#define SKINOS_STATUS_BYTES 24u
#define SKINOS_FRAME_BYTES 288u
#define SKINOS_MAGIC 0x53494e50u
#define SKINOS_SLOTS 16u
#define SKINOS_REPORT_BYTES 20u
#define SKINOS_HID_REPORT_BYTES SKINOS_WINE_REPORT_BYTES
#define SKINOS_MAX_READS 8u
#define SKINOS_TAG 0x70496b53u
#define KIND_CONTROL 1
#define KIND_BUS 2
#define KIND_CHILD 3

/* Little-endian wire u32s: magic, version, kind, slot, payload length,
 * vendor, product, device version; followed by exactly 256 payload bytes. */
enum frame_kind { FRAME_CREATE = 1, FRAME_REPORT = 2, FRAME_DESTROY = 3 };

/* Wine's pinned wdm.h omits this type. This is the exact definition shipped
 * by dlls/dinput/tests/driver_bus.c:705-710 at the pinned Wine commit. */
typedef struct _PNP_BUS_INFORMATION
{
    GUID BusTypeGuid;
    INTERFACE_TYPE LegacyBusType;
    ULONG BusNumber;
} PNP_BUS_INFORMATION;

struct extension
{
    ULONG kind;
    DEVICE_OBJECT *self;
    DEVICE_OBJECT *physical;
    DEVICE_OBJECT *lower;
    ULONG slot;
    ULONG generation;
    USHORT vendor, product, version;
    BOOLEAN removed, started;
    BOOLEAN cleanup_in_progress;
    BOOLEAN reported;
    BOOLEAN has_report, seen_report;
    UCHAR report[SKINOS_REPORT_BYTES];
    UCHAR wine_report[SKINOS_HID_REPORT_BYTES];
    LIST_ENTRY reads;
    ULONG read_count;
};

static DRIVER_OBJECT *bus_driver;
static DEVICE_OBJECT *control_device;
static DEVICE_OBJECT *root_device;
static DEVICE_OBJECT *children[SKINOS_SLOTS];
static KSPIN_LOCK devices_lock;
static ULONG next_generation;
static UNICODE_STRING control_link;
static KEVENT replay_stop;
static HANDLE replay_thread;
static BOOLEAN replay_stopping;
#define SKINOS_REPLAY_INTERVAL_MS 16

/* Keep the native-PE driver independent of an arbitrary userspace CRT. */
static void copy_bytes(void *dest, const void *src, SIZE_T length)
{
    UCHAR *d = dest;
    const UCHAR *s = src;
    while (length--) *d++ = *s++;
}
static void zero_bytes(void *dest, SIZE_T length)
{
    UCHAR *d = dest;
    while (length--) *d++ = 0;
}
static BOOLEAN equal_bytes(const void *a, const void *b, SIZE_T length)
{
    const UCHAR *x = a, *y = b;
    while (length--) if (*x++ != *y++) return FALSE;
    return TRUE;
}
static ULONG read_u32(const UCHAR *p)
{
    return (ULONG)p[0] | ((ULONG)p[1] << 8) | ((ULONG)p[2] << 16) | ((ULONG)p[3] << 24);
}
static void write_u32(UCHAR *p, ULONG value)
{
    p[0] = (UCHAR)value;
    p[1] = (UCHAR)(value >> 8);
    p[2] = (UCHAR)(value >> 16);
    p[3] = (UCHAR)(value >> 24);
}
static SIZE_T wide_length(const WCHAR *s)
{
    SIZE_T n = 0;
    while (s[n]) ++n;
    return n;
}
static WCHAR *append_wide(WCHAR *dst, const WCHAR *src)
{
    while (*src) *dst++ = *src++;
    *dst = 0;
    return dst;
}
static WCHAR *append_hex4(WCHAR *dst, USHORT value)
{
    static const WCHAR hex[] = L"0123456789ABCDEF";
    unsigned int shift = 16;
    while (shift) { shift -= 4; *dst++ = hex[(value >> shift) & 15]; }
    *dst = 0;
    return dst;
}
static WCHAR *append_slot(WCHAR *dst, ULONG slot)
{
    *dst++ = (WCHAR)('0' + slot / 10);
    *dst++ = (WCHAR)('0' + slot % 10);
    *dst = 0;
    return dst;
}
static NTSTATUS complete(IRP *irp, NTSTATUS status, ULONG_PTR bytes)
{
    irp->IoStatus.Status = status;
    irp->IoStatus.Information = bytes;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    return status;
}
static struct extension *extension(DEVICE_OBJECT *device)
{
    return device->DeviceExtension;
}
static void *pool(SIZE_T size)
{
    return ExAllocatePoolWithTag(NonPagedPool, size, SKINOS_TAG);
}

static NTSTATUS return_string(IRP *irp, const WCHAR *string, BOOLEAN multistring)
{
    SIZE_T size = (wide_length(string) + 1 + !!multistring) * sizeof(WCHAR);
    WCHAR *result = pool(size);
    if (!result) return complete(irp, STATUS_NO_MEMORY, 0);
    zero_bytes(result, size);
    copy_bytes(result, string, size - (!!multistring * sizeof(WCHAR)));
    return complete(irp, STATUS_SUCCESS, (ULONG_PTR)result);
}

static DEVICE_OBJECT *reference_child(ULONG slot, ULONG session)
{
    DEVICE_OBJECT *device;
    KIRQL irql;
    KeAcquireSpinLock(&devices_lock, &irql);
    if (session && (replay_stopping || !extension(control_device)->started || extension(control_device)->generation != session))
        device = NULL;
    else device = children[slot];
    if (device) ObfReferenceObject(device);
    KeReleaseSpinLock(&devices_lock, irql);
    return device;
}
static DEVICE_OBJECT *reference_root(void)
{
    DEVICE_OBJECT *device;
    KIRQL irql;
    KeAcquireSpinLock(&devices_lock, &irql);
    device = root_device;
    if (device) ObfReferenceObject(device);
    KeReleaseSpinLock(&devices_lock, irql);
    return device;
}
static void notify_relations(void)
{
    DEVICE_OBJECT *root = reference_root();
    if (!root) return;
    IoInvalidateDeviceRelations(extension(root)->physical, BusRelations);
    ObDereferenceObject(root);
}

/* All read-queue and last-report fields are protected by the cancel spinlock.
 * A queued IRP's cancel callback and a publisher cannot both claim that IRP. */
static void WINAPI cancel_read(DEVICE_OBJECT *device, IRP *irp)
{
    struct extension *ext = irp->Tail.Overlay.DriverContext[0];
    (void)device;
    RemoveEntryList(&irp->Tail.Overlay.ListEntry);
    --ext->read_count;
    IoReleaseCancelSpinLock(irp->CancelIrql);
    complete(irp, STATUS_CANCELLED, 0);
}
static void finish_list(LIST_ENTRY *list, NTSTATUS status, ULONG bytes)
{
    while (!IsListEmpty(list))
    {
        IRP *irp = CONTAINING_RECORD(RemoveHeadList(list), IRP, Tail.Overlay.ListEntry);
        complete(irp, status, bytes);
    }
}
static void remove_child_state(struct extension *ext)
{
    LIST_ENTRY done;
    KIRQL irql;
    InitializeListHead(&done);
    IoAcquireCancelSpinLock(&irql);
    ext->removed = TRUE;
    ext->has_report = FALSE;
    while (!IsListEmpty(&ext->reads))
    {
        IRP *irp = CONTAINING_RECORD(RemoveHeadList(&ext->reads), IRP, Tail.Overlay.ListEntry);
        IoSetCancelRoutine(irp, NULL);
        InsertTailList(&done, &irp->Tail.Overlay.ListEntry);
    }
    ext->read_count = 0;
    IoReleaseCancelSpinLock(irql);
    finish_list(&done, STATUS_DEVICE_NOT_CONNECTED, 0);
}
/* Returns TRUE only for a PDO removed before it appeared in any BusRelations.
 * PnP cannot remove such an unseen PDO, so its owner must delete it directly. */
static BOOLEAN unpublish_child(DEVICE_OBJECT *device)
{
    struct extension *ext = extension(device);
    BOOLEAN delete_unseen = FALSE;
    KIRQL irql;
    KeAcquireSpinLock(&devices_lock, &irql);
    if (children[ext->slot] == device)
    {
        children[ext->slot] = NULL;
        delete_unseen = !ext->reported;
    }
    KeReleaseSpinLock(&devices_lock, irql);
    remove_child_state(ext);
    return delete_unseen;
}
static void retire_child(DEVICE_OBJECT *device)
{
    if (unpublish_child(device)) IoDeleteDevice(device);
}
static void remove_all_children(void)
{
    ULONG i;
    for (i = 0; i < SKINOS_SLOTS; ++i)
    {
        DEVICE_OBJECT *child = reference_child(i, 0);
        if (!child) continue;
        retire_child(child);
        ObDereferenceObject(child);
    }
    notify_relations();
}
static NTSTATUS publish_report(struct extension *ext, const UCHAR *report)
{
    LIST_ENTRY done;
    BOOLEAN consumed = FALSE;
    KIRQL irql;
    InitializeListHead(&done);
    IoAcquireCancelSpinLock(&irql);
    if (ext->removed)
    {
        IoReleaseCancelSpinLock(irql);
        return STATUS_DEVICE_NOT_CONNECTED;
    }
    copy_bytes(ext->report, report, SKINOS_REPORT_BYTES);
    skinos_translate_report(ext->report, ext->wine_report);
    ext->seen_report = TRUE;
    while (!IsListEmpty(&ext->reads))
    {
        IRP *irp = CONTAINING_RECORD(RemoveHeadList(&ext->reads), IRP, Tail.Overlay.ListEntry);
        IoSetCancelRoutine(irp, NULL);
        /* The adapter uses only this genuine producer report. Both descriptors
         * have no report ID; hidclass owns its zero prefix. Never copy the
         * producer's six unused tail bytes into a HID-sized read buffer. */
        copy_bytes(irp->UserBuffer, ext->wine_report, SKINOS_HID_REPORT_BYTES);
        InsertTailList(&done, &irp->Tail.Overlay.ListEntry);
        consumed = TRUE;
    }
    ext->read_count = 0;
    ext->has_report = !consumed;
    IoReleaseCancelSpinLock(irql);
    finish_list(&done, STATUS_SUCCESS, SKINOS_HID_REPORT_BYTES);
    return STATUS_SUCCESS;
}
/* Refresh delivery to a newly acquired HID reader, without inventing another
 * producer report or changing cached state. In particular, do NOT take a local
 * snapshot and call publish_report(): a newer release might arrive meanwhile.
 * Claim IRPs and copy the CURRENT cache under the same cancel lock used by the
 * real producer/canceller; complete them only after releasing that lock. */
static void replay_pending_reads(struct extension *ext)
{
    LIST_ENTRY done;
    KIRQL irql;
    InitializeListHead(&done);
    IoAcquireCancelSpinLock(&irql);
    if (!ext->removed && ext->seen_report)
    {
        while (!IsListEmpty(&ext->reads))
        {
            IRP *irp = CONTAINING_RECORD(RemoveHeadList(&ext->reads), IRP, Tail.Overlay.ListEntry);
            IoSetCancelRoutine(irp, NULL);
            copy_bytes(irp->UserBuffer, ext->wine_report, SKINOS_HID_REPORT_BYTES);
            InsertTailList(&done, &irp->Tail.Overlay.ListEntry);
            --ext->read_count;
        }
    }
    IoReleaseCancelSpinLock(irql);
    finish_list(&done, STATUS_SUCCESS, SKINOS_HID_REPORT_BYTES);
}
static DEVICE_OBJECT *reference_replay_child(ULONG slot)
{
    DEVICE_OBJECT *device = NULL;
    KIRQL irql;
    KeAcquireSpinLock(&devices_lock, &irql);
    if (!replay_stopping && root_device && control_device &&
        !extension(root_device)->removed && extension(root_device)->started &&
        !extension(control_device)->removed && extension(control_device)->started &&
        !extension(control_device)->cleanup_in_progress)
    {
        device = children[slot];
        if (device) ObfReferenceObject(device);
    }
    KeReleaseSpinLock(&devices_lock, irql);
    return device;
}
static void WINAPI replay_worker(void *context)
{
    LARGE_INTEGER timeout;
    NTSTATUS status;
    (void)context;
    timeout.QuadPart = -(LONGLONG)SKINOS_REPLAY_INTERVAL_MS * 10000;
    for (;;)
    {
        status = KeWaitForSingleObject(&replay_stop, Executive, KernelMode, FALSE, &timeout);
        if (status != STATUS_TIMEOUT) break;
        for (ULONG slot = 0; slot < SKINOS_SLOTS; ++slot)
        {
            DEVICE_OBJECT *child = reference_replay_child(slot);
            if (!child) continue;
            /* The object reference prevents a destroyed generation from being
             * freed/reused while the cancel-lock check observes removal. */
            replay_pending_reads(extension(child));
            ObDereferenceObject(child);
        }
    }
    if (status != STATUS_SUCCESS)
    {
        KIRQL irql;
        KeAcquireSpinLock(&devices_lock, &irql);
        replay_stopping = TRUE;
        KeReleaseSpinLock(&devices_lock, irql);
        remove_all_children();
    }
    PsTerminateSystemThread(status);
}
static NTSTATUS start_replay_worker(void)
{
    OBJECT_ATTRIBUTES attributes;
    KeInitializeEvent(&replay_stop, NotificationEvent, FALSE);
    InitializeObjectAttributes(&attributes, NULL, OBJ_KERNEL_HANDLE, NULL, NULL);
    replay_stopping = FALSE;
    replay_thread = NULL;
    return PsCreateSystemThread(&replay_thread, SYNCHRONIZE, &attributes,
                                NULL, NULL, replay_worker, NULL);
}
static void stop_replay_worker(void)
{
    KIRQL irql;
    KeAcquireSpinLock(&devices_lock, &irql);
    replay_stopping = TRUE;
    KeReleaseSpinLock(&devices_lock, irql);
    if (!replay_thread) return;
    KeSetEvent(&replay_stop, IO_NO_INCREMENT, FALSE);
    /* This private synchronizable handle is never closed anywhere else.
     * Joining outside both driver locks is the unload/rundown barrier. Wine's
     * KeFlushQueuedDpcs is a stub and deliberately is not used. */
    NTSTATUS status = ZwWaitForSingleObject(replay_thread, FALSE, NULL);
    if (status != STATUS_SUCCESS)
    {
        LARGE_INTEGER retry;
        retry.QuadPart = -160000;
        DbgPrint("SkinOS input: worker join failed %#lx; refusing unsafe unload\n", status);
        /* An unexpected wait failure must not free code under a live worker.
         * Fail closed, with a sleeping retry rather than an unchecked unload. */
        do { KeDelayExecutionThread(KernelMode, FALSE, &retry);
             status = ZwWaitForSingleObject(replay_thread, FALSE, NULL); }
        while (status != STATUS_SUCCESS);
    }
    ZwClose(replay_thread);
    replay_thread = NULL;
}
static NTSTATUS queue_read(struct extension *ext, IRP *irp, ULONG size)
{
    NTSTATUS status;
    KIRQL irql;
    ULONG bytes = 0;
    if (size < SKINOS_HID_REPORT_BYTES || !irp->UserBuffer)
        return complete(irp, STATUS_BUFFER_TOO_SMALL, 0);
    IoAcquireCancelSpinLock(&irql);
    if (ext->removed) status = STATUS_DEVICE_NOT_CONNECTED;
    else if (irp->Cancel) status = STATUS_CANCELLED;
    else if (ext->has_report)
    {
        copy_bytes(irp->UserBuffer, ext->wine_report, SKINOS_HID_REPORT_BYTES);
        ext->has_report = FALSE;
        status = STATUS_SUCCESS;
        bytes = SKINOS_HID_REPORT_BYTES;
    }
    else if (ext->read_count >= SKINOS_MAX_READS) status = STATUS_INSUFFICIENT_RESOURCES;
    else
    {
        irp->Tail.Overlay.DriverContext[0] = ext;
        IoMarkIrpPending(irp);
        IoSetCancelRoutine(irp, cancel_read);
        InsertTailList(&ext->reads, &irp->Tail.Overlay.ListEntry);
        ++ext->read_count;
        IoReleaseCancelSpinLock(irql);
        return STATUS_PENDING;
    }
    IoReleaseCancelSpinLock(irql);
    return complete(irp, status, bytes);
}

static NTSTATUS create_child(ULONG slot, USHORT vendor, USHORT product, USHORT version, ULONG session)
{
    DEVICE_OBJECT *root = reference_root(), *device;
    struct extension *ext;
    NTSTATUS status;
    KIRQL irql;
    if (!root) return STATUS_DEVICE_NOT_READY;
    status = IoCreateDevice(bus_driver, sizeof(*ext), NULL, FILE_DEVICE_UNKNOWN, 0, FALSE, &device);
    if (status) { ObDereferenceObject(root); return status; }
    ext = extension(device);
    zero_bytes(ext, sizeof(*ext));
    ext->kind = KIND_CHILD;
    ext->self = device;
    ext->slot = slot;
    ext->vendor = vendor;
    ext->product = product;
    ext->version = version;
    InitializeListHead(&ext->reads);
    /* Valve creates the output before it sends the first changed report.
     * A newly attached gamepad is neutral; initialize the HID representation
     * explicitly (its neutral hat is not an arbitrary all-zero byte buffer).
     * All subsequent state comes exclusively from mapped producer reports. */
    skinos_translate_report(ext->report, ext->wine_report);
    ext->has_report = TRUE;
    ext->seen_report = TRUE;
    device->Flags |= DO_POWER_PAGABLE;
    device->Flags &= ~DO_DEVICE_INITIALIZING;
    KeAcquireSpinLock(&devices_lock, &irql);
    if (replay_stopping || root_device != root || extension(root)->removed || !extension(root)->started ||
        !extension(control_device)->started || extension(control_device)->generation != session)
        status = STATUS_DEVICE_NOT_READY;
    else if (children[slot]) status = STATUS_OBJECT_NAME_COLLISION;
    else
    {
        if (!++next_generation) ++next_generation;
        ext->generation = next_generation;
        children[slot] = device;
        status = STATUS_SUCCESS;
    }
    KeReleaseSpinLock(&devices_lock, irql);
    if (status) IoDeleteDevice(device);
    else IoInvalidateDeviceRelations(extension(root)->physical, BusRelations);
    ObDereferenceObject(root);
    return status;
}

static NTSTATUS WINAPI dispatch_ioctl(DEVICE_OBJECT *device, IRP *irp)
{
    IO_STACK_LOCATION *stack = IoGetCurrentIrpStackLocation(irp);
    const UCHAR *frame = irp->AssociatedIrp.SystemBuffer;
    ULONG kind, slot, length, vendor, product, version;
    ULONG session;
    ULONG code = stack->Parameters.DeviceIoControl.IoControlCode;
    DEVICE_OBJECT *child;
    NTSTATUS status;
    if (extension(device)->kind != KIND_CONTROL ||
        (code != SKINOS_IOCTL_FRAME && code != SKINOS_IOCTL_STATUS))
        return complete(irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    if (!stack->FileObject || !(session = (ULONG)(ULONG_PTR)stack->FileObject->FsContext))
        return complete(irp, STATUS_DEVICE_NOT_CONNECTED, 0);
    if (code == SKINOS_IOCTL_STATUS)
    {
        ULONG values[6] = {SKINOS_MAGIC, 1, 0, 0, 0, 0}, i;
        KIRQL irql;
        if (stack->Parameters.DeviceIoControl.InputBufferLength)
            return complete(irp, STATUS_INVALID_PARAMETER, 0);
        if (stack->Parameters.DeviceIoControl.OutputBufferLength < SKINOS_STATUS_BYTES || !frame)
            return complete(irp, STATUS_BUFFER_TOO_SMALL, 0);
        KeAcquireSpinLock(&devices_lock, &irql);
        if (replay_stopping || !extension(device)->started || extension(device)->generation != session)
        {
            KeReleaseSpinLock(&devices_lock, irql);
            return complete(irp, STATUS_DEVICE_NOT_CONNECTED, 0);
        }
        if (root_device)
        {
            values[2] = 1;
            values[3] = !!extension(root_device)->started;
            values[4] = !!extension(root_device)->removed;
        }
        for (i = 0; i < SKINOS_SLOTS; ++i)
            if (children[i]) values[5] |= 1u << i;
        KeReleaseSpinLock(&devices_lock, irql);
        for (i = 0; i < 6; ++i)
            write_u32((UCHAR *)irp->AssociatedIrp.SystemBuffer + i * 4, values[i]);
        return complete(irp, STATUS_SUCCESS, SKINOS_STATUS_BYTES);
    }
    if (stack->Parameters.DeviceIoControl.InputBufferLength != SKINOS_FRAME_BYTES || !frame ||
        read_u32(frame) != SKINOS_MAGIC || read_u32(frame + 4) != 1)
        return complete(irp, STATUS_INVALID_PARAMETER, 0);
    kind = read_u32(frame + 8);
    slot = read_u32(frame + 12);
    length = read_u32(frame + 16);
    vendor = read_u32(frame + 20);
    product = read_u32(frame + 24);
    version = read_u32(frame + 28);
    if (slot >= SKINOS_SLOTS || vendor > 0xffff || product > 0xffff || version > 0xffff)
        return complete(irp, STATUS_INVALID_PARAMETER, 0);
    if (kind == FRAME_CREATE)
    {
        if (length != sizeof(skinos_input_descriptor) ||
            !equal_bytes(frame + 32, skinos_input_descriptor, length))
            return complete(irp, STATUS_INVALID_PARAMETER, 0);
        status = create_child(slot, (USHORT)vendor, (USHORT)product, (USHORT)version, session);
    }
    else if ((kind == FRAME_REPORT && length == SKINOS_REPORT_BYTES) ||
             (kind == FRAME_DESTROY && length == 0))
    {
        if (!(child = reference_child(slot, session))) return complete(irp, STATUS_NO_SUCH_DEVICE, 0);
        if (kind == FRAME_REPORT) status = publish_report(extension(child), frame + 32);
        else
        {
            retire_child(child);
            notify_relations();
            status = STATUS_SUCCESS;
        }
        ObDereferenceObject(child);
    }
    else status = STATUS_INVALID_PARAMETER;
    return complete(irp, status, 0);
}

static NTSTATUS copy_ioctl_result(IRP *irp, ULONG size, const void *data, ULONG length)
{
    if (size < length || !irp->UserBuffer) return complete(irp, STATUS_BUFFER_TOO_SMALL, 0);
    copy_bytes(irp->UserBuffer, data, length);
    return complete(irp, STATUS_SUCCESS, length);
}
static void serial_string(struct extension *ext, WCHAR *value)
{
    WCHAR *end = append_slot(append_wide(value, L"SkinOS-Wine-Output-"), ext->slot);
    end = append_hex4(append_wide(end, L"-"), (USHORT)(ext->generation >> 16));
    append_hex4(end, (USHORT)ext->generation);
}
static NTSTATUS WINAPI dispatch_internal_ioctl(DEVICE_OBJECT *device, IRP *irp)
{
    IO_STACK_LOCATION *stack = IoGetCurrentIrpStackLocation(irp);
    struct extension *ext = extension(device);
    ULONG code = stack->Parameters.DeviceIoControl.IoControlCode;
    ULONG size = stack->Parameters.DeviceIoControl.OutputBufferLength;
    HID_DEVICE_ATTRIBUTES attrs;
    HID_DESCRIPTOR desc;
    WCHAR string[64];
    const WCHAR *text;
    KIRQL irql;
    if (ext->kind != KIND_CHILD) return complete(irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    if (ext->removed) return complete(irp, STATUS_DEVICE_NOT_CONNECTED, 0);
    switch (code)
    {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
        zero_bytes(&desc, sizeof(desc));
        desc.bLength = sizeof(desc);
        desc.bDescriptorType = 0x21;
        desc.bcdHID = 0x0111;
        desc.bNumDescriptors = 1;
        desc.DescriptorList[0].bReportType = 0x22;
        desc.DescriptorList[0].wReportLength = sizeof(skinos_wine_descriptor);
        return copy_ioctl_result(irp, size, &desc, sizeof(desc));
    case IOCTL_HID_GET_REPORT_DESCRIPTOR:
        return copy_ioctl_result(irp, size, skinos_wine_descriptor, sizeof(skinos_wine_descriptor));
    case IOCTL_HID_GET_DEVICE_ATTRIBUTES:
        zero_bytes(&attrs, sizeof(attrs));
        attrs.Size = sizeof(attrs);
        attrs.VendorID = ext->vendor;
        attrs.ProductID = ext->product;
        attrs.VersionNumber = ext->version;
        return copy_ioctl_result(irp, size, &attrs, sizeof(attrs));
    case IOCTL_HID_GET_STRING:
        switch ((ULONG_PTR)stack->Parameters.DeviceIoControl.Type3InputBuffer & 0xffff)
        {
        case HID_STRING_ID_IMANUFACTURER: text = L"SkinOS"; break;
        case HID_STRING_ID_IPRODUCT: text = L"SkinOS mapped Steam Input output"; break;
        case HID_STRING_ID_ISERIALNUMBER: serial_string(ext, string); text = string; break;
        default: return complete(irp, STATUS_INVALID_PARAMETER, 0);
        }
        return copy_ioctl_result(irp, size, text, (ULONG)((wide_length(text) + 1) * sizeof(WCHAR)));
    case IOCTL_HID_READ_REPORT:
        return queue_read(ext, irp, size);
    case IOCTL_HID_GET_INPUT_REPORT:
    {
        HID_XFER_PACKET *packet = irp->UserBuffer;
        NTSTATUS status;
        if (size < sizeof(*packet) || !packet || !packet->reportBuffer || packet->reportId ||
            packet->reportBufferLen < SKINOS_HID_REPORT_BYTES)
            return complete(irp, STATUS_INVALID_PARAMETER, 0);
        IoAcquireCancelSpinLock(&irql);
        if (ext->removed) status = STATUS_DEVICE_NOT_CONNECTED;
        else if (!ext->seen_report) status = STATUS_DEVICE_NOT_READY;
        else { copy_bytes(packet->reportBuffer, ext->wine_report, SKINOS_HID_REPORT_BYTES); status = STATUS_SUCCESS; }
        IoReleaseCancelSpinLock(irql);
        return complete(irp, status, status ? 0 : SKINOS_HID_REPORT_BYTES);
    }
    case IOCTL_HID_ACTIVATE_DEVICE:
    case IOCTL_HID_DEACTIVATE_DEVICE:
        return complete(irp, STATUS_SUCCESS, 0);
    /* The verified producer descriptor contains no output/feature reports.
     * Never acknowledge rumble that was not delivered to a genuine API. */
    case IOCTL_HID_WRITE_REPORT:
    case IOCTL_HID_SET_OUTPUT_REPORT:
    case IOCTL_HID_GET_FEATURE:
    case IOCTL_HID_SET_FEATURE:
    case IOCTL_GET_PHYSICAL_DESCRIPTOR:
    case IOCTL_HID_GET_INDEXED_STRING:
        return complete(irp, STATUS_NOT_SUPPORTED, 0);
    default: return complete(irp, STATUS_NOT_SUPPORTED, 0);
    }
}

static NTSTATUS query_child_id(struct extension *ext, IRP *irp, BUS_QUERY_ID_TYPE type)
{
    WCHAR value[96], *end;
    static const WCHAR compat[] = L"WINEBUS\\WINE_COMP_XINPUT\0WINEBUS\\WINE_COMP_HID\0";
    switch (type)
    {
    case BusQueryDeviceID:
    case BusQueryHardwareIDs:
        end = append_hex4(append_wide(value, L"WINEBUS\\VID_"), ext->vendor);
        end = append_hex4(append_wide(end, L"&PID_"), ext->product);
        append_slot(append_wide(end, L"&MI_"), ext->slot);
        return return_string(irp, value, type == BusQueryHardwareIDs);
    case BusQueryInstanceID:
        serial_string(ext, value);
        return return_string(irp, value, FALSE);
    case BusQueryCompatibleIDs:
    {
        void *result = pool(sizeof(compat));
        if (!result) return complete(irp, STATUS_NO_MEMORY, 0);
        copy_bytes(result, compat, sizeof(compat));
        return complete(irp, STATUS_SUCCESS, (ULONG_PTR)result);
    }
    default: return complete(irp, irp->IoStatus.Status, irp->IoStatus.Information);
    }
}
static NTSTATUS bus_relations(DEVICE_OBJECT *device, IRP *irp)
{
    DEVICE_RELATIONS *old = (DEVICE_RELATIONS *)irp->IoStatus.Information;
    DEVICE_RELATIONS *relations;
    ULONG count = old ? old->Count : 0, i;
    SIZE_T size;
    KIRQL irql;
    if (count > 256) return complete(irp, STATUS_INVALID_PARAMETER, irp->IoStatus.Information);
    size = FIELD_OFFSET(DEVICE_RELATIONS, Objects[count + SKINOS_SLOTS]);
    if (!(relations = pool(size))) return complete(irp, STATUS_NO_MEMORY, irp->IoStatus.Information);
    relations->Count = count;
    if (old) copy_bytes(relations->Objects, old->Objects, count * sizeof(DEVICE_OBJECT *));
    KeAcquireSpinLock(&devices_lock, &irql);
    for (i = 0; i < SKINOS_SLOTS; ++i)
    {
        /* A late query on a retired root must not enumerate replacement
         * root children from the shared slot table. */
        if (root_device != device || extension(device)->removed) break;
        if (!children[i]) continue;
        extension(children[i])->reported = TRUE;
        ObfReferenceObject(children[i]);
        relations->Objects[relations->Count++] = children[i];
    }
    KeReleaseSpinLock(&devices_lock, irql);
    if (old) ExFreePool(old);
    return complete(irp, STATUS_SUCCESS, (ULONG_PTR)relations);
}
static NTSTATUS WINAPI start_completion(DEVICE_OBJECT *device, IRP *irp, void *context)
{
    struct extension *ext = context;
    KIRQL irql;
    (void)device;
    KeAcquireSpinLock(&devices_lock, &irql);
    if (NT_SUCCESS(irp->IoStatus.Status) && !ext->removed) ext->started = TRUE;
    KeReleaseSpinLock(&devices_lock, irql);
    if (irp->PendingReturned) IoMarkIrpPending(irp);
    return STATUS_SUCCESS;
}
static NTSTATUS WINAPI dispatch_pnp(DEVICE_OBJECT *device, IRP *irp)
{
    struct extension *ext = extension(device);
    IO_STACK_LOCATION *stack = IoGetCurrentIrpStackLocation(irp);
    ULONG minor = stack->MinorFunction;
    KIRQL irql;
    if (ext->kind == KIND_BUS)
    {
        if (minor == IRP_MN_START_DEVICE)
        {
            IoCopyCurrentIrpStackLocationToNext(irp);
            IoSetCompletionRoutine(irp, start_completion, ext, TRUE, TRUE, TRUE);
            return IoCallDriver(ext->lower, irp);
        }
        if (minor == IRP_MN_STOP_DEVICE)
        {
            KeAcquireSpinLock(&devices_lock, &irql);
            ext->started = FALSE;
            KeReleaseSpinLock(&devices_lock, irql);
        }
        if (minor == IRP_MN_QUERY_DEVICE_RELATIONS && stack->Parameters.QueryDeviceRelations.Type == BusRelations)
            return bus_relations(device, irp);
        if (minor == IRP_MN_SURPRISE_REMOVAL || minor == IRP_MN_REMOVE_DEVICE)
        {
            BOOLEAN owns_root;
            KeAcquireSpinLock(&devices_lock, &irql);
            ext->removed = TRUE;
            owns_root = root_device == device;
            KeReleaseSpinLock(&devices_lock, irql);
            if (owns_root)
            {
                /* Keep the old root reserved while its children are swept.
                 * AddDevice cannot publish a replacement until this finishes.
                 * A later REMOVE after SURPRISE_REMOVAL no longer owns the
                 * root and therefore must not sweep replacement children. */
                remove_all_children();
                KeAcquireSpinLock(&devices_lock, &irql);
                if (root_device == device) root_device = NULL;
                KeReleaseSpinLock(&devices_lock, irql);
            }
        }
        if (minor == IRP_MN_REMOVE_DEVICE)
        {
            NTSTATUS status;
            irp->IoStatus.Status = STATUS_SUCCESS;
            IoSkipCurrentIrpStackLocation(irp);
            status = IoCallDriver(ext->lower, irp);
            IoDetachDevice(ext->lower);
            IoDeleteDevice(device);
            return status;
        }
        IoSkipCurrentIrpStackLocation(irp);
        return IoCallDriver(ext->lower, irp);
    }
    if (ext->kind != KIND_CHILD) return complete(irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    switch (minor)
    {
    case IRP_MN_START_DEVICE: ext->started = TRUE; return complete(irp, STATUS_SUCCESS, 0);
    case IRP_MN_STOP_DEVICE: ext->started = FALSE; return complete(irp, STATUS_SUCCESS, 0);
    case IRP_MN_QUERY_STOP_DEVICE:
    case IRP_MN_CANCEL_STOP_DEVICE:
    case IRP_MN_QUERY_REMOVE_DEVICE:
    case IRP_MN_CANCEL_REMOVE_DEVICE:
        return complete(irp, STATUS_SUCCESS, 0);
    case IRP_MN_SURPRISE_REMOVAL:
        unpublish_child(device);
        return complete(irp, STATUS_SUCCESS, 0);
    case IRP_MN_REMOVE_DEVICE:
        unpublish_child(device);
        complete(irp, STATUS_SUCCESS, 0);
        IoDeleteDevice(device);
        return STATUS_SUCCESS;
    case IRP_MN_QUERY_ID:
        return query_child_id(ext, irp, stack->Parameters.QueryId.IdType);
    case IRP_MN_QUERY_DEVICE_TEXT:
        return return_string(irp, L"SkinOS mapped Steam Input output", FALSE);
    case IRP_MN_QUERY_CAPABILITIES:
    {
        DEVICE_CAPABILITIES *caps = stack->Parameters.DeviceCapabilities.Capabilities;
        ULONG i;
        if (!caps || caps->Version != 1 || caps->Size < sizeof(*caps))
            return complete(irp, STATUS_INVALID_PARAMETER, 0);
        caps->Removable = TRUE;
        caps->SurpriseRemovalOK = TRUE;
        caps->UniqueID = TRUE;
        caps->Address = ext->slot;
        caps->UINumber = ext->slot;
        for (i = 0; i < PowerSystemMaximum; ++i) caps->DeviceState[i] = PowerDeviceD0;
        return complete(irp, STATUS_SUCCESS, 0);
    }
    case IRP_MN_QUERY_BUS_INFORMATION:
    {
        PNP_BUS_INFORMATION *info = pool(sizeof(*info));
        if (!info) return complete(irp, STATUS_NO_MEMORY, 0);
        zero_bytes(info, sizeof(*info));
        info->LegacyBusType = PNPBus;
        return complete(irp, STATUS_SUCCESS, (ULONG_PTR)info);
    }
    case IRP_MN_QUERY_DEVICE_RELATIONS:
        if (stack->Parameters.QueryDeviceRelations.Type == TargetDeviceRelation)
        {
            DEVICE_RELATIONS *relations = pool(sizeof(*relations));
            if (!relations) return complete(irp, STATUS_NO_MEMORY, 0);
            relations->Count = 1;
            relations->Objects[0] = device;
            ObfReferenceObject(device);
            return complete(irp, STATUS_SUCCESS, (ULONG_PTR)relations);
        }
        break;
    case IRP_MN_QUERY_PNP_DEVICE_STATE:
        return complete(irp, STATUS_SUCCESS, irp->IoStatus.Information);
    }
    return complete(irp, irp->IoStatus.Status, irp->IoStatus.Information);
}
static NTSTATUS WINAPI dispatch_power(DEVICE_OBJECT *device, IRP *irp)
{
    struct extension *ext = extension(device);
    PoStartNextPowerIrp(irp);
    if (ext->kind == KIND_BUS)
    {
        IoSkipCurrentIrpStackLocation(irp);
        return PoCallDriver(ext->lower, irp);
    }
    return complete(irp, STATUS_SUCCESS, 0);
}
static NTSTATUS WINAPI dispatch_open_close(DEVICE_OBJECT *device, IRP *irp)
{
    struct extension *ext = extension(device);
    IO_STACK_LOCATION *stack = IoGetCurrentIrpStackLocation(irp);
    BOOLEAN cleanup = FALSE;
    KIRQL irql;
    if (ext->removed) return complete(irp, STATUS_DEVICE_NOT_CONNECTED, 0);
    if (ext->kind == KIND_CONTROL)
    {
        if (!stack->FileObject) return complete(irp, STATUS_INVALID_PARAMETER, 0);
        KeAcquireSpinLock(&devices_lock, &irql);
        if (stack->MajorFunction == IRP_MJ_CREATE)
        {
            if (replay_stopping)
            {
                KeReleaseSpinLock(&devices_lock, irql);
                return complete(irp, STATUS_DEVICE_NOT_READY, 0);
            }
            if (ext->started || ext->cleanup_in_progress)
            {
                KeReleaseSpinLock(&devices_lock, irql);
                return complete(irp, STATUS_SHARING_VIOLATION, 0);
            }
            if (!++ext->generation) ++ext->generation;
            stack->FileObject->FsContext = (void *)(ULONG_PTR)ext->generation;
            ext->started = TRUE;
        }
        /* Wine dispatch_close sends IRP_MJ_CLOSE without a preceding CLEANUP.
         * Both are valid terminal ownership boundaries; generation guards make
         * a later CLOSE after CLEANUP idempotent. */
        if ((stack->MajorFunction == IRP_MJ_CLEANUP || stack->MajorFunction == IRP_MJ_CLOSE) && ext->started &&
            (ULONG)(ULONG_PTR)stack->FileObject->FsContext == ext->generation)
        {
            /* Reject racing CREATE/report IRPs from the closed producer even
             * if a later helper opens a fresh exclusive control connection. */
            ext->cleanup_in_progress = TRUE;
            ext->started = FALSE;
            cleanup = TRUE;
        }
        KeReleaseSpinLock(&devices_lock, irql);
        if (cleanup)
        {
            remove_all_children();
            KeAcquireSpinLock(&devices_lock, &irql);
            ext->cleanup_in_progress = FALSE;
            KeReleaseSpinLock(&devices_lock, irql);
        }
    }
    return complete(irp, STATUS_SUCCESS, 0);
}
static NTSTATUS WINAPI dispatch_unsupported(DEVICE_OBJECT *device, IRP *irp)
{
    (void)device;
    return complete(irp, STATUS_INVALID_DEVICE_REQUEST, 0);
}
static NTSTATUS WINAPI add_device(DRIVER_OBJECT *driver, DEVICE_OBJECT *physical)
{
    DEVICE_OBJECT *device;
    struct extension *ext;
    KIRQL irql;
    NTSTATUS status = IoCreateDevice(driver, sizeof(*ext), NULL, FILE_DEVICE_BUS_EXTENDER, 0, FALSE, &device);
    if (status) return status;
    ext = extension(device);
    zero_bytes(ext, sizeof(*ext));
    ext->kind = KIND_BUS;
    ext->self = device;
    ext->physical = physical;
    ext->lower = IoAttachDeviceToDeviceStack(device, physical);
    if (!ext->lower) { IoDeleteDevice(device); return STATUS_NO_SUCH_DEVICE; }
    KeAcquireSpinLock(&devices_lock, &irql);
    if (replay_stopping) status = STATUS_DEVICE_NOT_READY;
    else if (root_device) status = STATUS_OBJECT_NAME_COLLISION;
    else { root_device = device; status = STATUS_SUCCESS; }
    KeReleaseSpinLock(&devices_lock, irql);
    if (status) { IoDetachDevice(ext->lower); IoDeleteDevice(device); return status; }
    device->Flags |= DO_POWER_PAGABLE;
    device->Flags &= ~DO_DEVICE_INITIALIZING;
    return STATUS_SUCCESS;
}
static void WINAPI unload(DRIVER_OBJECT *driver)
{
    (void)driver;
    stop_replay_worker();
    remove_all_children();
    IoDeleteSymbolicLink(&control_link);
    if (control_device) IoDeleteDevice(control_device);
}
NTSTATUS WINAPI DriverEntry(DRIVER_OBJECT *driver, UNICODE_STRING *registry_path)
{
    UNICODE_STRING name;
    struct extension *ext;
    NTSTATUS status;
    ULONG i;
    (void)registry_path;
    bus_driver = driver;
    KeInitializeSpinLock(&devices_lock);
    for (i = 0; i <= IRP_MJ_MAXIMUM_FUNCTION; ++i) driver->MajorFunction[i] = dispatch_unsupported;
    driver->MajorFunction[IRP_MJ_CREATE] = dispatch_open_close;
    driver->MajorFunction[IRP_MJ_CLOSE] = dispatch_open_close;
    driver->MajorFunction[IRP_MJ_CLEANUP] = dispatch_open_close;
    driver->MajorFunction[IRP_MJ_DEVICE_CONTROL] = dispatch_ioctl;
    driver->MajorFunction[IRP_MJ_INTERNAL_DEVICE_CONTROL] = dispatch_internal_ioctl;
    driver->MajorFunction[IRP_MJ_PNP] = dispatch_pnp;
    driver->MajorFunction[IRP_MJ_POWER] = dispatch_power;
    driver->DriverExtension->AddDevice = add_device;
    driver->DriverUnload = unload;
    RtlInitUnicodeString(&name, L"\\Device\\SkinOsInputBus");
    status = IoCreateDevice(driver, sizeof(*ext), &name, FILE_DEVICE_UNKNOWN, FILE_DEVICE_SECURE_OPEN, TRUE, &control_device);
    if (status) return status;
    ext = extension(control_device);
    zero_bytes(ext, sizeof(*ext));
    ext->kind = KIND_CONTROL;
    ext->self = control_device;
    RtlInitUnicodeString(&control_link, L"\\DosDevices\\SkinOsInputBus");
    status = IoCreateSymbolicLink(&control_link, &name);
    if (status) { IoDeleteDevice(control_device); control_device = NULL; return status; }
    control_device->Flags |= DO_BUFFERED_IO;
    control_device->Flags &= ~DO_DEVICE_INITIALIZING;
    status = start_replay_worker();
    if (status)
    {
        IoDeleteSymbolicLink(&control_link);
        IoDeleteDevice(control_device);
        control_device = NULL;
        return status;
    }
    return STATUS_SUCCESS;
}
