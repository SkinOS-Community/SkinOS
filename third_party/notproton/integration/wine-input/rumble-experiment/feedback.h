#ifndef SKINOS_RUMBLE_EXPERIMENT_FEEDBACK_H
#define SKINOS_RUMBLE_EXPERIMENT_FEEDBACK_H
#include <stdint.h>
#define SKINOS_FEEDBACK_IOCTL 0x0022a008u
#define SKINOS_FEEDBACK_BINDING_IOCTL 0x0022a00cu
#define SKINOS_FEEDBACK_MAGIC 0x53495242u
#define SKINOS_FEEDBACK_VERSION 1u
#define SKINOS_FEEDBACK_CAPACITY 64u
/* Exactly twelve LE u32s; supported x64/ARM64 Windows are little endian.
 * This is an actual accepted PDO HID output, not a native SDK delivery ACK. */
typedef struct SkinosFeedbackFrame {
    uint32_t magic,version,kind,byte_size;
    uint32_t control_session,slot,generation,sequence_low,sequence_high;
    uint32_t left,right,report_id;
} SkinosFeedbackFrame;
_Static_assert(sizeof(SkinosFeedbackFrame)==48,"feedback wire layout");
typedef struct SkinosFeedbackBinding {uint32_t magic,version,byte_size,control_session,slot,generation;} SkinosFeedbackBinding;
_Static_assert(sizeof(SkinosFeedbackBinding)==24,"binding wire layout");
typedef struct SkinosFeedbackQueue {
    SkinosFeedbackFrame frames[SKINOS_FEEDBACK_CAPACITY];
    uint32_t count;
    uint64_t sequence;
} SkinosFeedbackQueue;
/* Caller holds its own device/session lock. No hidden allocation or wait. */
static inline int skinos_feedback_push(SkinosFeedbackQueue*q,uint32_t session,uint32_t slot,uint32_t generation,uint16_t left,uint16_t right){
    if(!session||slot>=16||!generation||q->count>=SKINOS_FEEDBACK_CAPACITY||q->sequence==UINT64_MAX)return 0;
    uint64_t sequence=++q->sequence;
    SkinosFeedbackFrame*f=&q->frames[q->count++];
    f->magic=SKINOS_FEEDBACK_MAGIC;f->version=1;f->kind=1;f->byte_size=48;
    f->control_session=session;f->slot=slot;f->generation=generation;
    f->sequence_low=(uint32_t)sequence;f->sequence_high=(uint32_t)(sequence>>32);
    f->left=left;f->right=right;f->report_id=1;return 1;
}
static inline void skinos_feedback_purge(SkinosFeedbackQueue*q,uint32_t slot,uint32_t generation){
    uint32_t dest=0;
    for(uint32_t i=0;i<q->count;i++){
        if(q->frames[i].slot==slot&&q->frames[i].generation==generation)continue;
        q->frames[dest++]=q->frames[i];
    }
    q->count=dest;
}
static inline int skinos_feedback_pop(SkinosFeedbackQueue*q,SkinosFeedbackFrame*out){
    if(!q->count)return 0;
    *out=q->frames[0];--q->count;
    for(uint32_t i=0;i<q->count;i++)q->frames[i]=q->frames[i+1];
    return 1;
}
#endif
