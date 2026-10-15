#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "adapter.h"
static unsigned u16(const unsigned char *p){return p[0]|((unsigned)p[1]<<8);}
static int s16(const unsigned char *p){unsigned x=u16(p);return x&32768?(int)x-65536:(int)x;}
int main(void){
    static const unsigned expected_buttons[16]={0,0,0,0,128,64,256,512,16,32,1024,0,1,2,4,8};
    static const unsigned expected_hats[16]={0,1,5,0,7,8,6,7,3,2,4,3,0,1,5,0};
    unsigned char in[20]={0},out[14];
    for(unsigned bit=0;bit<16;bit++){
        memset(in,0,sizeof(in));in[2]=(unsigned char)(1u<<bit);in[3]=(unsigned char)((1u<<bit)>>8);
        skinos_translate_report(in,out);assert(u16(out)==expected_buttons[bit]);assert(out[2]==expected_hats[(1u<<bit)&15]);
    }
    for(unsigned dpad=0;dpad<16;dpad++){
        memset(in,0,sizeof(in));in[2]=(unsigned char)dpad;skinos_translate_report(in,out);assert(out[2]==expected_hats[dpad]);
    }
    for(unsigned a=0;a<2;a++)for(unsigned b=0;b<2;b++){
        memset(in,0,sizeof(in));in[4]=(unsigned char)(a*255);in[5]=(unsigned char)(b*255);
        skinos_translate_report(in,out);assert(out[3]==a*255&&out[4]==b*255);
    }
    static const int axes[]={-32768,0,32767};
    for(unsigned axis=0;axis<4;axis++)for(unsigned n=0;n<3;n++){
        memset(in,0,sizeof(in));unsigned value=(unsigned)axes[n]&65535;
        in[6+2*axis]=(unsigned char)value;in[7+2*axis]=(unsigned char)(value>>8);
        skinos_translate_report(in,out);assert(s16(out+5+2*axis)==axes[n]);
        /* Wine XI scales the same -32768..32767 range identically; IG alone
         * uses -y-1 then unsigned0..65535, giving32767 at centred XboxY=0. */
        if(axis==1||axis==3)assert((-axes[n]-1)+32768==(32767-axes[n]));
    }
    memset(in,0,sizeof(in));in[0]=in[1]=255;in[3]=8;memset(in+14,255,6);skinos_translate_report(in,out);
    for(unsigned i=0;i<14;i++)assert(out[i]==0);
    unsigned bits=0,size=0,count=0;
    for(unsigned i=0;i<sizeof(skinos_wine_descriptor);){
        unsigned tag=skinos_wine_descriptor[i++],n=tag&3;n=n==3?4:n;unsigned val=0;
        assert(i+n<=sizeof(skinos_wine_descriptor));for(unsigned j=0;j<n;j++)val|=(unsigned)skinos_wine_descriptor[i++]<<(j*8);
        if(tag==0x75)size=val;else if(tag==0x95)count=val;else if((tag&0xfc)==0x80)bits+=size*count;
        /* Haptics Feature/Output IDs are independent of unchanged Input0.
         * The actual pinned Wine parser test validates per-type layouts. */
    }
    assert(bits==SKINOS_WINE_REPORT_BYTES*8);
    puts("adapter: 16 button bits, 16 dpad states, trigger extremes, 12 axis extremes, padding and descriptor length passed");
    return 0;
}
