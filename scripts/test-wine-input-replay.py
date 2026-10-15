#!/usr/bin/env python3
"""Offline queue tests of extracted SkinOS driver C; never runs Wine or Steam.

Fixtures model existing producer reports. They do not stand in for a successful
Steam API call, driver load, or end-to-end input delivery. Generated artifacts
are confined to target/wine-input-replay-experiment/tests.
"""
from __future__ import annotations

import hashlib
import json
import pathlib
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
DRIVER = ROOT / "third_party/notproton/integration/wine-input/driver.c"
OUT = ROOT / "target/wine-input-replay-experiment/tests"
FUNCTIONS = (
    "cancel_read", "finish_list", "publish_report", "queue_read",
    "replay_pending_reads", "start_replay_worker", "stop_replay_worker",
)


def extract_function(source: str, name: str) -> str:
    """Extract a real static definition, balancing braces outside C literals."""
    pattern = re.compile(r"^static\s+[^;{}]*\b" + re.escape(name) + r"\s*\([^;{}]*\)\s*\{", re.M)
    matches = list(pattern.finditer(source))
    if len(matches) != 1:
        raise RuntimeError(f"Expected one real definition of {name}; found {len(matches)}")
    start = matches[0].start()
    pos = source.index("{", matches[0].start())
    depth = 0
    state = "code"
    while pos < len(source):
        char = source[pos]
        pair = source[pos:pos + 2]
        if state == "line":
            if char == "\n":
                state = "code"
        elif state == "block":
            if pair == "*/":
                state = "code"
                pos += 1
        elif state in ("string", "char"):
            if char == "\\":
                pos += 1
            elif char == ('"' if state == "string" else "'"):
                state = "code"
        elif pair == "//":
            state = "line"
            pos += 1
        elif pair == "/*":
            state = "block"
            pos += 1
        elif char in ('"', "'"):
            state = "string" if char == '"' else "char"
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[start:pos + 1]
        pos += 1
    raise RuntimeError(f"Unterminated function {name}")


PRELUDE = r'''
#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "adapter.h"

#define WINAPI
#define TRUE 1
#define FALSE 0
#define SKINOS_REPORT_BYTES 20u
#define SKINOS_HID_REPORT_BYTES SKINOS_WINE_REPORT_BYTES
#define SKINOS_MAX_READS 8u
#define STATUS_SUCCESS ((NTSTATUS)0)
#define STATUS_PENDING ((NTSTATUS)0x103)
#define STATUS_CANCELLED ((NTSTATUS)0xc0000120u)
#define STATUS_DEVICE_NOT_CONNECTED ((NTSTATUS)0xc000009du)
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xc0000023u)
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xc000009au)
#define STATUS_NO_MEMORY ((NTSTATUS)0xc0000017u)
#define STATUS_INVALID_HANDLE ((NTSTATUS)0xc0000008u)
#define REQUIRE(expr) do { if (!(expr)) { \
    fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #expr); exit(1); \
} } while (0)

typedef uint8_t UCHAR, BOOLEAN;
typedef uint32_t ULONG;
typedef uintptr_t ULONG_PTR;
typedef size_t SIZE_T;
typedef int KIRQL;
typedef int32_t NTSTATUS;
typedef struct list_entry { struct list_entry *Flink, *Blink; } LIST_ENTRY;
typedef struct device_object { int unused; } DEVICE_OBJECT;
typedef struct irp IRP;
typedef void (*PDRIVER_CANCEL)(DEVICE_OBJECT *, IRP *);
struct irp {
    struct { struct { void *DriverContext[4]; LIST_ENTRY ListEntry; } Overlay; } Tail;
    void *UserBuffer;
    BOOLEAN Cancel;
    KIRQL CancelIrql;
    PDRIVER_CANCEL CancelRoutine;
    unsigned pending, completions;
    NTSTATUS final_status;
    ULONG_PTR final_bytes;
    UCHAR buffer[SKINOS_HID_REPORT_BYTES + 8];
};
struct extension {
    BOOLEAN removed, has_report, seen_report;
    UCHAR report[SKINOS_REPORT_BYTES];
    UCHAR wine_report[SKINOS_HID_REPORT_BYTES];
    LIST_ENTRY reads;
    ULONG read_count;
};

#define CONTAINING_RECORD(ptr, type, field) ((type *)((char *)(ptr) - offsetof(type, field)))
static void InitializeListHead(LIST_ENTRY *head) { head->Flink = head->Blink = head; }
static int IsListEmpty(const LIST_ENTRY *head) { return head->Flink == head; }
static void RemoveEntryList(LIST_ENTRY *entry) {
    REQUIRE(entry->Flink && entry->Blink);
    entry->Blink->Flink = entry->Flink; entry->Flink->Blink = entry->Blink;
    entry->Flink = entry->Blink = NULL;
}
static LIST_ENTRY *RemoveHeadList(LIST_ENTRY *head) {
    LIST_ENTRY *entry = head->Flink; REQUIRE(entry != head); RemoveEntryList(entry); return entry;
}
static void InsertTailList(LIST_ENTRY *head, LIST_ENTRY *entry) {
    entry->Flink = head; entry->Blink = head->Blink;
    head->Blink->Flink = entry; head->Blink = entry;
}

static int cancel_lock, other_lock;
static unsigned completed_total, copies, cancel_changes;
static void (*after_unlock)(void);
static void IoAcquireCancelSpinLock(KIRQL *previous) {
    REQUIRE(!cancel_lock); *previous = 0; cancel_lock = 1;
}
static void IoReleaseCancelSpinLock(KIRQL previous) {
    void (*hook)(void);
    REQUIRE(cancel_lock && previous == 0);
    cancel_lock = 0;
    hook = after_unlock; after_unlock = NULL;
    if (hook) hook();
}
static PDRIVER_CANCEL IoSetCancelRoutine(IRP *irp, PDRIVER_CANCEL routine) {
    PDRIVER_CANCEL old;
    REQUIRE(cancel_lock);
    old = irp->CancelRoutine; irp->CancelRoutine = routine; ++cancel_changes;
    return old;
}
static void IoMarkIrpPending(IRP *irp) { REQUIRE(cancel_lock); irp->pending = 1; }
static void copy_bytes(void *dest, const void *src, SIZE_T count) {
    REQUIRE(cancel_lock); ++copies; memcpy(dest, src, count);
}
static NTSTATUS complete(IRP *irp, NTSTATUS status, ULONG_PTR bytes) {
    REQUIRE(!cancel_lock && !other_lock);
    REQUIRE(irp->completions == 0);
    REQUIRE(!irp->CancelRoutine);
    for (unsigned i = SKINOS_HID_REPORT_BYTES; i < sizeof(irp->buffer); ++i)
        REQUIRE(irp->buffer[i] == 0xa5);
    ++irp->completions; ++completed_total;
    irp->final_status = status; irp->final_bytes = bytes;
    return status;
}

/* This second seam checks actual start/stop functions, not a running worker.
 * No host thread, wait, signal, or kernel handle is created by these mocks. */
typedef int64_t LONGLONG;
typedef struct { LONGLONG QuadPart; } LARGE_INTEGER;
typedef struct { int signaled; } KEVENT;
typedef struct { ULONG flags; } OBJECT_ATTRIBUTES;
typedef void *HANDLE;
typedef int KSPIN_LOCK;
#define SYNCHRONIZE 0x00100000u
#define OBJ_KERNEL_HANDLE 0x200u
#define NotificationEvent 0
#define KernelMode 0
#define IO_NO_INCREMENT 0
#define InitializeObjectAttributes(ptr, name, attrs, root, security) ((ptr)->flags = (attrs))
static KEVENT replay_stop;
static HANDLE replay_thread;
static BOOLEAN replay_stopping;
static KSPIN_LOCK devices_lock;
static unsigned thread_creates, stop_signals, join_waits, join_delays, handle_closes, join_warnings;
static unsigned failing_waits;
static NTSTATUS thread_create_result;
static void replay_worker(void *context) { (void)context; REQUIRE(0); }
static void KeAcquireSpinLock(KSPIN_LOCK *lock, KIRQL *previous) {
    REQUIRE(lock == &devices_lock && !other_lock && !cancel_lock);
    *previous = 0; other_lock = 1;
}
static void KeReleaseSpinLock(KSPIN_LOCK *lock, KIRQL previous) {
    REQUIRE(lock == &devices_lock && other_lock && previous == 0); other_lock = 0;
}
static void KeInitializeEvent(KEVENT *event, int type, BOOLEAN signaled) {
    REQUIRE(event == &replay_stop && type == NotificationEvent && !signaled);
    REQUIRE(!other_lock && !cancel_lock); event->signaled = signaled;
}
static NTSTATUS PsCreateSystemThread(HANDLE *thread, ULONG access, OBJECT_ATTRIBUTES *attrs,
                                    HANDLE process, void *client_id, void (*entry)(void *), void *context) {
    REQUIRE(!other_lock && !cancel_lock && thread == &replay_thread);
    REQUIRE(access == SYNCHRONIZE && attrs->flags == OBJ_KERNEL_HANDLE);
    REQUIRE(process == NULL && client_id == NULL && entry == replay_worker && context == NULL);
    ++thread_creates;
    if (!thread_create_result) *thread = (HANDLE)(uintptr_t)0x1234;
    return thread_create_result;
}
static int KeSetEvent(KEVENT *event, int increment, BOOLEAN wait) {
    REQUIRE(!other_lock && !cancel_lock && replay_stopping);
    REQUIRE(event == &replay_stop && increment == IO_NO_INCREMENT && !wait);
    event->signaled = TRUE; ++stop_signals; return 0;
}
static NTSTATUS ZwWaitForSingleObject(HANDLE thread, BOOLEAN alertable, LARGE_INTEGER *timeout) {
    REQUIRE(!other_lock && !cancel_lock && replay_stopping && replay_stop.signaled);
    REQUIRE(thread == replay_thread && thread == (HANDLE)(uintptr_t)0x1234);
    REQUIRE(!alertable && timeout == NULL && handle_closes == 0);
    ++join_waits;
    if (failing_waits) { --failing_waits; return STATUS_INVALID_HANDLE; }
    return STATUS_SUCCESS;
}
static NTSTATUS KeDelayExecutionThread(int mode, BOOLEAN alertable, LARGE_INTEGER *delay) {
    REQUIRE(!other_lock && !cancel_lock && mode == KernelMode && !alertable);
    REQUIRE(delay && delay->QuadPart < 0); ++join_delays; return STATUS_SUCCESS;
}
static ULONG DbgPrint(const char *format, ...) { REQUIRE(format != NULL); ++join_warnings; return 0; }
static NTSTATUS ZwClose(HANDLE handle) {
    REQUIRE(!other_lock && !cancel_lock && handle == replay_thread && !failing_waits);
    REQUIRE(join_waits > 0 && handle_closes == 0); ++handle_closes; return STATUS_SUCCESS;
}
'''


TESTS = r'''
static void reset_extension(struct extension *ext) {
    REQUIRE(!cancel_lock && !other_lock && !after_unlock);
    memset(ext, 0, sizeof(*ext)); InitializeListHead(&ext->reads);
}
static void reset_irp(IRP *irp) {
    memset(irp, 0, sizeof(*irp)); memset(irp->buffer, 0xa5, sizeof(irp->buffer));
    irp->UserBuffer = irp->buffer;
}
static void fixture(UCHAR report[20], int released) {
    memset(report, 0, 20);
    if (!released) {
        report[2] = 0x11; report[3] = 0x94; report[4] = 201; report[5] = 79;
        report[6] = 0x34; report[7] = 0x12; report[8] = 0x67; report[9] = 0x85;
        report[10] = 0x7f; report[11] = 0x7f; report[12] = 0x55; report[13] = 0xaa;
    }
    /* Deliberately different padding tests preservation of the source cache. */
    report[0] = released ? 0x42 : 0x21; report[1] = released ? 0x17 : 0xe1;
    for (unsigned i = 14; i < 20; ++i) report[i] = (UCHAR)(i + (released ? 17 : 83));
}
static void seed_consumed_snapshot(struct extension *ext, const UCHAR source[20]) {
    memcpy(ext->report, source, 20); skinos_translate_report(source, ext->wine_report);
    ext->seen_report = TRUE; ext->has_report = FALSE;
}
static void expect_pending(struct extension *ext, IRP *irp) {
    reset_irp(irp);
    REQUIRE(queue_read(ext, irp, SKINOS_HID_REPORT_BYTES) == STATUS_PENDING);
    REQUIRE(irp->pending && !irp->completions && irp->CancelRoutine == cancel_read);
}
static void cancel_pending(IRP *irp) {
    KIRQL previous;
    PDRIVER_CANCEL callback;
    IoAcquireCancelSpinLock(&previous);
    irp->Cancel = TRUE; irp->CancelIrql = previous;
    callback = IoSetCancelRoutine(irp, NULL);
    REQUIRE(callback != NULL);
    callback(NULL, irp);
    REQUIRE(!cancel_lock && irp->completions == 1);
    REQUIRE(irp->final_status == STATUS_CANCELLED && irp->final_bytes == 0);
}
static void expect_success(const IRP *irp, const UCHAR source[20]) {
    UCHAR expected[SKINOS_HID_REPORT_BYTES];
    skinos_translate_report(source, expected);
    REQUIRE(irp->completions == 1 && irp->final_status == STATUS_SUCCESS);
    REQUIRE(irp->final_bytes == SKINOS_HID_REPORT_BYTES);
    REQUIRE(memcmp(irp->buffer, expected, sizeof(expected)) == 0);
}
static void expect_cache(const struct extension *ext, const UCHAR source[20]) {
    UCHAR expected[SKINOS_HID_REPORT_BYTES];
    skinos_translate_report(source, expected);
    REQUIRE(memcmp(ext->report, source, 20) == 0);
    REQUIRE(memcmp(ext->wine_report, expected, sizeof(expected)) == 0);
}
static void test_no_snapshot(void) {
    struct extension ext; IRP pending; unsigned before;
    reset_extension(&ext); expect_pending(&ext, &pending); before = copies;
    replay_pending_reads(&ext);
    REQUIRE(!ext.seen_report && !ext.has_report && ext.read_count == 1);
    REQUIRE(!pending.completions && copies == before);
    cancel_pending(&pending);
    puts("PASS no genuine snapshot: replay leaves read pending");
}
static void test_removed(void) {
    struct extension ext; IRP pending; UCHAR source[20]; unsigned before;
    fixture(source, 0); reset_extension(&ext); seed_consumed_snapshot(&ext, source);
    expect_pending(&ext, &pending); ext.removed = TRUE; before = copies;
    replay_pending_reads(&ext);
    REQUIRE(ext.read_count == 1 && !pending.completions && copies == before);
    expect_cache(&ext, source); cancel_pending(&pending);
    puts("PASS removed device: no replay or cache mutation");
}
static void test_no_pending(void) {
    struct extension ext; UCHAR source[20]; unsigned before;
    fixture(source, 0); reset_extension(&ext);
    REQUIRE(publish_report(&ext, source) == STATUS_SUCCESS);
    REQUIRE(ext.has_report); before = copies;
    replay_pending_reads(&ext);
    REQUIRE(ext.has_report && ext.seen_report && ext.read_count == 0 && copies == before);
    expect_cache(&ext, source);
    puts("PASS no pending reads: latest snapshot preserved");
}
static void test_exact_cached_bytes(void) {
    struct extension ext; IRP reads[3], repeated; UCHAR source[20]; unsigned before;
    fixture(source, 0); reset_extension(&ext); seed_consumed_snapshot(&ext, source);
    for (unsigned i = 0; i < 3; ++i) expect_pending(&ext, &reads[i]);
    before = completed_total; replay_pending_reads(&ext);
    REQUIRE(completed_total == before + 3 && ext.read_count == 0 && IsListEmpty(&ext.reads));
    REQUIRE(ext.seen_report && !ext.has_report);
    for (unsigned i = 0; i < 3; ++i) expect_success(&reads[i], source);
    expect_cache(&ext, source);
    expect_pending(&ext, &repeated); replay_pending_reads(&ext);
    expect_success(&repeated, source); expect_cache(&ext, source);
    puts("PASS exact cached bytes: all pending reads and repeated read, canaries intact");
}
static struct extension *race_ext;
static UCHAR race_release[20];
static void publish_release_on_unlock(void) {
    REQUIRE(!cancel_lock);
    REQUIRE(publish_report(race_ext, race_release) == STATUS_SUCCESS);
}
static void test_newer_release_wins(void) {
    struct extension ext; IRP old_read, new_read; UCHAR pressed[20];
    fixture(pressed, 0); fixture(race_release, 1);
    reset_extension(&ext); seed_consumed_snapshot(&ext, pressed); expect_pending(&ext, &old_read);
    race_ext = &ext; after_unlock = publish_release_on_unlock;
    replay_pending_reads(&ext);
    REQUIRE(after_unlock == NULL && !cancel_lock);
    expect_success(&old_read, pressed); expect_cache(&ext, race_release);
    REQUIRE(ext.has_report && ext.read_count == 0);
    reset_irp(&new_read);
    REQUIRE(queue_read(&ext, &new_read, SKINOS_HID_REPORT_BYTES) == STATUS_SUCCESS);
    expect_success(&new_read, race_release); expect_cache(&ext, race_release);
    REQUIRE(!ext.has_report);
    puts("PASS unlock interleaving: newer release remains cached and reaches next read");
}
static void test_cancel_before_replay(void) {
    struct extension ext; IRP cancelled, survivor; UCHAR source[20]; unsigned before;
    fixture(source, 0); reset_extension(&ext); seed_consumed_snapshot(&ext, source);
    expect_pending(&ext, &cancelled); expect_pending(&ext, &survivor);
    cancel_pending(&cancelled); REQUIRE(ext.read_count == 1);
    before = completed_total; replay_pending_reads(&ext);
    REQUIRE(completed_total == before + 1 && cancelled.completions == 1);
    REQUIRE(cancelled.final_status == STATUS_CANCELLED && cancelled.final_bytes == 0);
    for (unsigned i = 0; i < sizeof(cancelled.buffer); ++i) REQUIRE(cancelled.buffer[i] == 0xa5);
    expect_success(&survivor, source); expect_cache(&ext, source);
    REQUIRE(ext.read_count == 0 && IsListEmpty(&ext.reads));
    puts("PASS cancel before replay: no copy or second completion of cancelled IRP");
}
static void reset_lifecycle(void) {
    REQUIRE(!other_lock && !cancel_lock);
    thread_creates = stop_signals = join_waits = join_delays = handle_closes = join_warnings = 0;
    failing_waits = 0; thread_create_result = STATUS_SUCCESS;
    replay_stopping = FALSE; replay_thread = NULL; replay_stop.signaled = FALSE;
}
static void test_stop_join(void) {
    reset_lifecycle(); REQUIRE(start_replay_worker() == STATUS_SUCCESS);
    REQUIRE(thread_creates == 1 && replay_thread && !replay_stopping);
    stop_replay_worker();
    REQUIRE(replay_stopping && !replay_thread && stop_signals == 1 && join_waits == 1);
    REQUIRE(handle_closes == 1 && join_delays == 0 && join_warnings == 0);
    puts("PASS worker stop: signaled and joined unlocked before handle close");
}
static void test_stop_without_thread(void) {
    reset_lifecycle(); stop_replay_worker();
    REQUIRE(replay_stopping && !replay_thread && !stop_signals && !join_waits && !handle_closes);
    puts("PASS no worker handle: stop needs no signal or wait");
}
static void test_start_failure(void) {
    reset_lifecycle(); thread_create_result = STATUS_NO_MEMORY;
    REQUIRE(start_replay_worker() == STATUS_NO_MEMORY && !replay_thread);
    stop_replay_worker(); REQUIRE(replay_stopping && !stop_signals && !join_waits && !handle_closes);
    puts("PASS thread creation failure: no invalid handle wait or close");
}
static void test_join_retry(void) {
    reset_lifecycle(); REQUIRE(start_replay_worker() == STATUS_SUCCESS);
    failing_waits = 2; stop_replay_worker();
    REQUIRE(join_waits == 3 && join_delays == 2 && join_warnings == 1);
    REQUIRE(handle_closes == 1 && !replay_thread && replay_stopping);
    puts("PASS transient join errors: retries unlocked before close, no premature rundown");
}
int main(void) {
    test_no_snapshot(); test_removed(); test_no_pending(); test_exact_cached_bytes();
    test_newer_release_wins(); test_cancel_before_replay();
    test_stop_join(); test_stop_without_thread(); test_start_failure(); test_join_retry();
    REQUIRE(!cancel_lock && !other_lock && !after_unlock);
    REQUIRE(copies > 0 && cancel_changes > 0);
    printf("OFFLINE ONLY: 6 replay and 4 worker lifecycle cases passed; no Wine, Steam, or real worker.\n");
    return 0;
}
'''


def main() -> None:
    source = DRIVER.read_text()
    extracted = {name: extract_function(source, name) for name in FUNCTIONS}
    compiler = shutil.which("clang")
    if not compiler:
        raise RuntimeError("A host clang compiler is required; Wine is never used")
    OUT.mkdir(parents=True, exist_ok=True)
    harness = OUT / "replay-extracted.c"
    binary = OUT / "replay-extracted"
    harness.write_text(PRELUDE + "\n\n" + "\n\n".join(extracted.values()) + "\n\n" + TESTS)
    command = [compiler, "-std=c11", "-O1", "-g", "-Wall", "-Wextra", "-Werror",
               "-fsanitize=address,undefined", "-fno-omit-frame-pointer",
               "-I", str(DRIVER.parent), str(harness), "-o", str(binary)]
    subprocess.run(command, check=True, cwd=ROOT)
    result = subprocess.run([str(binary)], check=True, capture_output=True, text=True, cwd=ROOT)
    print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="")
    report = {
        "schema_version": 1,
        "scope": "offline extracted C queue tests, not Wine/Steam execution",
        "driver_sha256": hashlib.sha256(source.encode()).hexdigest(),
        "function_sha256": {key: hashlib.sha256(value.encode()).hexdigest() for key, value in extracted.items()},
        "compiler_command": command,
        "cases_passed": 10,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }
    (OUT / "report.json").write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
