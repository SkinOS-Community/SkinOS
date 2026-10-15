#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "feedback.h"
int main(void){
    SkinosFeedbackQueue q={0};SkinosFeedbackFrame f;
    assert(!skinos_feedback_pop(&q,&f));
    assert(!skinos_feedback_push(&q,0,0,1,1,1));
    assert(!skinos_feedback_push(&q,1,16,1,1,1));
    assert(!skinos_feedback_push(&q,1,0,0,1,1));
    assert(q.count==0&&q.sequence==0);
    assert(skinos_feedback_push(&q,7,3,100,0x1234,0xabcd));
    assert(skinos_feedback_push(&q,7,0,101,0xffff,1));
    assert(skinos_feedback_push(&q,7,3,102,0,0));
    skinos_feedback_purge(&q,3,100);assert(q.count==2);
    assert(skinos_feedback_pop(&q,&f));assert(f.control_session==7&&f.slot==0&&f.generation==101);
    assert(f.left==0xffff&&f.right==1&&f.sequence_low==2&&f.sequence_high==0);
    assert(skinos_feedback_pop(&q,&f));assert(f.slot==3&&f.generation==102&&f.left==0&&f.right==0&&f.sequence_low==3);
    assert(!skinos_feedback_pop(&q,&f));
    for(unsigned i=0;i<SKINOS_FEEDBACK_CAPACITY;i++)assert(skinos_feedback_push(&q,8,1,103,(uint16_t)i,0));
    SkinosFeedbackQueue before=q;
    assert(!skinos_feedback_push(&q,8,1,103,65535,65535));assert(!memcmp(&q,&before,sizeof(q)));
    skinos_feedback_purge(&q,1,102);assert(q.count==SKINOS_FEEDBACK_CAPACITY);
    skinos_feedback_purge(&q,1,103);assert(q.count==0);
    q.sequence=0xffffffffu;assert(skinos_feedback_push(&q,9,0,104,1,2));
    assert(skinos_feedback_pop(&q,&f));assert(f.sequence_low==0&&f.sequence_high==1);
    assert(f.magic==SKINOS_FEEDBACK_MAGIC&&f.version==1&&f.kind==1&&f.byte_size==48&&f.report_id==1);
    q.sequence=UINT64_MAX;assert(!skinos_feedback_push(&q,9,0,104,1,2));assert(q.count==0);
    puts("feedback queue: exact fields, reverse slots/generations, purge, overflow, FIFO, explicit zero and sequence exhaustion passed (offline only)");
}
