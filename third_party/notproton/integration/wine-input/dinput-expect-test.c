#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "dinput-expect.h"
int main(void){
    uint8_t in[20]={0};SkinosDiState s;
    memset(&s,0xee,sizeof(s));skinos_di_expect(in,&s);
    assert(s.x==32768&&s.y==32767&&s.rx==32768&&s.ry==32767&&s.z==32768&&s.pov==0xffffffffu);
    for(unsigned i=0;i<10;i++)assert(s.buttons[i]==0);
    SkinosDiState api_center=s;api_center.y=api_center.ry=32768;
    assert(!skinos_di_equal(&api_center,&s));assert(skinos_di_equal_semantic(&api_center,&s,in));
    api_center.y=0;assert(!skinos_di_equal_semantic(&api_center,&s,in));
    api_center=s;api_center.y=32766;assert(!skinos_di_equal_semantic(&api_center,&s,in));
    api_center=s;api_center.buttons[0]=128;assert(!skinos_di_equal_semantic(&api_center,&s,in));
    in[8]=1;skinos_di_expect(in,&s);api_center=s;api_center.y=32768;
    assert(!skinos_di_equal_semantic(&api_center,&s,in));memset(in,0,20);
    static const int own_button[16]={-1,-1,-1,-1,7,6,8,9,4,5,-1,-1,0,1,2,3};
    for(unsigned bit=0;bit<16;bit++){
        memset(in,0,20);in[2]=(uint8_t)(1u<<bit);in[3]=(uint8_t)((1u<<bit)>>8);skinos_di_expect(in,&s);
        for(unsigned i=0;i<10;i++)assert(s.buttons[i]==((int)i==own_button[bit]?128:0));
    }
    static const uint32_t pov[16]={0xffffffffu,0,18000,0xffffffffu,27000,31500,22500,27000,9000,4500,13500,9000,0xffffffffu,0,18000,0xffffffffu};
    for(unsigned i=0;i<16;i++){memset(in,0,20);in[2]=i;skinos_di_expect(in,&s);assert(s.pov==pov[i]);}
    memset(in,0,20);in[4]=255;skinos_di_expect(in,&s);assert(s.z==65408);
    in[4]=0;in[5]=255;skinos_di_expect(in,&s);assert(s.z==128);
    in[4]=255;skinos_di_expect(in,&s);assert(s.z==32768);
    static const int source[]={-32768,0,32767};
    static const int expected_x[]={0,32768,65535},expected_y[]={65535,32767,0};
    for(unsigned a=0;a<4;a++)for(unsigned n=0;n<3;n++){
        memset(in,0,20);unsigned value=(unsigned)source[n]&65535;in[6+2*a]=value;in[7+2*a]=value>>8;skinos_di_expect(in,&s);
        const int values[]={s.x,s.y,s.rx,s.ry};assert(values[a]==((a==1||a==3)?expected_y[n]:expected_x[n]));
    }
    memset(in,0xff,20);skinos_di_expect(in,&s);memset(in,0,20);skinos_di_expect(in,&s);
    assert(s.x==32768&&s.y==32767&&s.z==32768&&s.rx==32768&&s.ry==32767&&s.pov==0xffffffffu);
    for(unsigned i=0;i<10;i++)assert(s.buttons[i]==0);
    puts("DI oracle: button/POV tables, known ranges and centre values, combined triggers, signed extremes and strict release passed");return 0;
}
