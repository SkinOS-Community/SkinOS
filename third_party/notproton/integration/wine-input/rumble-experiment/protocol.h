#ifndef SKINOS_WINE_INPUT_PROTOCOL_H
#define SKINOS_WINE_INPUT_PROTOCOL_H
#include <stdint.h>
#define SKINOS_INPUT_MAGIC 0x53494e50u
#define SKINOS_INPUT_VERSION 1u
#define SKINOS_INPUT_IOCTL 0x0022a000u
#define SKINOS_INPUT_STATUS_IOCTL 0x0022a004u
#define SKINOS_INPUT_SLOTS 16u
typedef struct SkinosInputFrame {
    uint32_t magic, version, kind, slot, payload_len, vendor, product, device_version;
    uint8_t payload[256];
} SkinosInputFrame;
_Static_assert(sizeof(SkinosInputFrame) == 288, "wire frame layout");
typedef struct SkinosInputStatus {
    uint32_t magic,version,root_present,root_started,root_removed,children_mask;
} SkinosInputStatus;
_Static_assert(sizeof(SkinosInputStatus)==24,"status layout");

/* Pure lifecycle policy shared with host-only tests. No Windows API is mocked
 * as successful input: this governs which real callbacks may be attempted. */
typedef struct SkinosEnsureSnapshot {
    uint32_t service_present,root_present,identity_verified;
    uint32_t file_present,file_equal,owner_busy;
} SkinosEnsureSnapshot;
enum SkinosEnsureAction { SKINOS_ENSURE_REUSE,SKINOS_ENSURE_REGISTER,SKINOS_ENSURE_REPLACE,SKINOS_ENSURE_BUSY,SKINOS_ENSURE_FOREIGN };
static inline enum SkinosEnsureAction skinos_ensure_action(const SkinosEnsureSnapshot*s){
    if(s->owner_busy)return SKINOS_ENSURE_BUSY;
    if(!s->identity_verified)return SKINOS_ENSURE_FOREIGN;
    /* An orphan different SYS with neither owned root nor service has no
     * ownership proof and must not be overwritten based on its filename. */
    if(!s->service_present&&!s->root_present&&s->file_present&&!s->file_equal)return SKINOS_ENSURE_FOREIGN;
    if(s->service_present&&s->root_present&&s->file_equal)return SKINOS_ENSURE_REUSE;
    if((s->service_present||s->root_present)&&!s->file_equal)return SKINOS_ENSURE_REPLACE;
    return SKINOS_ENSURE_REGISTER;
}
typedef struct SkinosEnsureOperations {
    uint32_t (*remove_owned)(void*);
    uint32_t (*register_payload)(void*);
    uint32_t (*verify_installed)(void*);
    uint32_t (*activate)(void*);
    uint32_t (*feed)(void*);
} SkinosEnsureOperations;
static inline uint32_t skinos_ensure_run(const SkinosEnsureSnapshot*s,const SkinosEnsureOperations*ops,void*context){
    enum SkinosEnsureAction action=skinos_ensure_action(s);uint32_t error;
    if(action==SKINOS_ENSURE_BUSY)return 32; /* ERROR_SHARING_VIOLATION */
    if(action==SKINOS_ENSURE_FOREIGN)return 5; /* ERROR_ACCESS_DENIED */
    if(action==SKINOS_ENSURE_REPLACE&&(error=ops->remove_owned(context)))return error;
    if(action!=SKINOS_ENSURE_REUSE&&(error=ops->register_payload(context)))return error;
    if((error=ops->verify_installed(context)))return error;
    if((error=ops->activate(context)))return error;
    return ops->feed(context);
}
#endif
