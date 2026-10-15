// Explicitly opted-in QA helper. Ordinary probe runs never dispatch vibration.
// Calls ONLY genuine SteamInput006 TriggerVibration, never SDL_Rumble or a
// fake Steam API. A dispatched void call is not callback/physical proof.
#ifndef SKINOS_INPUT_RUMBLE_QA_H
#define SKINOS_INPUT_RUMBLE_QA_H
struct RumbleQaTarget {
    U64 handle, steamid;
    U32 appid, fixture_id;
    int slot;
    SteamUtils009Prefix *utils;
    GetXInputState xget;
    bool stop_pending;
};
static RumbleQaTarget rumble_qa_target;

static bool rumble_qa_identity() {
    const RumbleQaTarget &t=rumble_qa_target;
    if(!context.input_initialized||!context.sdk_initialized||!t.utils||t.utils->GetAppID()!=t.appid)return false;
    U64 actual=0;int online=0;
    return !skinos_client_identity(context.client,context.user,context.pipe,&actual,&online)&&online&&actual==t.steamid;
}
static bool rumble_qa_route() {
    if(!rumble_qa_identity())return false;
    RumbleQaTarget &t=rumble_qa_target;
    U64 handles[16]={0};int count=context.input->GetConnectedControllers(handles);
    if(count!=1||!handles[0])return false;
    if(t.handle&&handles[0]!=t.handle)return false;
    int slot=context.input->GetGamepadIndexForController(handles[0]);
    if(slot<0||slot>=4||(t.handle&&slot!=t.slot)||context.input->GetControllerForGamepadIndex(slot)!=handles[0])return false;
    U32 connected=0;
    for(U32 i=0;i<4;++i) {
        XInputState state={};U32 result=t.xget?t.xget(i,&state):1167;
        if(result!=0&&result!=1167)return false;
        if(result==0) {
            ++connected;
            if((int)i!=slot)return false;
            const GamepadState &p=state.pad;
            // This QA fixture stays neutral; unrelated input is never used to
            // select a rumble target. Actual mapper centre rounding is +/-1.
            if(p.buttons||p.left_trigger||p.right_trigger||p.left_x < -1||p.left_x > 1||
               p.left_y < -1||p.left_y > 1||p.right_x < -1||p.right_x > 1||p.right_y < -1||p.right_y > 1)return false;
        }
    }
    if(connected!=1)return false;
    t.handle=handles[0];t.slot=slot;return true;
}

static void input_rumble_qa_cleanup() {
    RumbleQaTarget &t=rumble_qa_target;
    if(!t.stop_pending)return;
    t.stop_pending=false;
    bool dispatched=false;
    // On failure stop only the old, still-connected genuine handle. Never
    // redirect zero to a newly attached controller or manufacture success.
    if(t.handle&&rumble_qa_identity()) {
        U64 handles[16]={0};int count=context.input->GetConnectedControllers(handles);
        if(count>=0&&count<=16)for(int i=0;i<count;++i)if(handles[i]==t.handle) {
            context.input->TriggerVibration(t.handle,0,0);dispatched=true;break;
        }
    }
    out("{\"event\":\"steam_input_rumble_cleanup\",\"zero_dispatched\":");boolean(dispatched);
    out(",\"callback_verified\":false,\"physical_device_verified\":false}\n");
}

static void run_input_rumble_qa(SteamUtils009Prefix *utils,GetXInputState xget,U32 appid,U64 steamid) {
    char flag[8],fixture[16],id[32];
    U32 n=GetEnvironmentVariableA("SKINOS_INPUT_RUMBLE_TEST",flag,sizeof(flag));
    if(!n)return;
    if(n!=1||flag[0]!='1')fail("invalid_rumble_test_opt_in");
    if(env("SKINOS_STEAM_INPUT_FIXTURE",fixture,sizeof(fixture))!=7||fixture[0]!='n'||fixture[1]!='e'||fixture[2]!='u'||
       fixture[3]!='t'||fixture[4]!='r'||fixture[5]!='a'||fixture[6]!='l')fail("rumble_requires_owned_neutral_fixture");
    if(!env("SKINOS_INPUT_RUMBLE_FIXTURE_ID",id,sizeof(id))||!decimal(id)||decimal(id)>0xffffffffULL)fail("rumble_fixture_attestation_missing");
    if(!context.sdk_initialized||!xget)fail("rumble_requires_real_game_sdk_and_xinput");
    rumble_qa_target={0,steamid,appid,(U32)decimal(id),-1,utils,xget,false};
    if(!rumble_qa_route())fail("rumble_fixture_route_not_unique_and_neutral");
    out("{\"event\":\"steam_input_rumble_route\",\"appid\":");number(appid);
    out(",\"identity_verified\":true,\"appid_verified\":true,\"reciprocal_route_verified\":true,\"fixture_id_from_parent\":");number(rumble_qa_target.fixture_id);
    out(",\"steam_input_handle\":\"");number(rumble_qa_target.handle);out("\",\"slot\":");signed_number(rumble_qa_target.slot);out("}\n");
    static const unsigned short motors[4][2]={{0x4000,0},{0,0},{0,0x6000},{0,0}};
    U32 began=GetTickCount();
    for(U32 phase=0;phase<4;++phase) {
        if(!rumble_qa_route())fail("rumble_route_changed_before_dispatch");
        rumble_qa_target.stop_pending=true;
        context.input->TriggerVibration(rumble_qa_target.handle,motors[phase][0],motors[phase][1]);
        out("{\"event\":\"steam_input_rumble_dispatch\",\"phase\":");number(phase);
        out(",\"left\":");number(motors[phase][0]);out(",\"right\":");number(motors[phase][1]);
        out(",\"elapsed_ms\":");number(GetTickCount()-began);
        out(",\"sdk_dispatched\":true,\"callback_verified\":false,\"physical_device_verified\":false}\n");
        U32 phase_began=GetTickCount();
        do {
            if(context.sdk_callbacks)context.sdk_callbacks();
            context.input->RunFrame(true);
            if(!rumble_qa_route())fail("rumble_route_changed_during_observation");
            if(GetTickCount()-began>5000)fail("rumble_test_deadline_exceeded");
            Sleep(25);
        }while(GetTickCount()-phase_began<500);
    }
    // The final explicit zero was sent and identity stayed valid through its
    // dwell. The parent still must observe the original SDL callback sequence.
    rumble_qa_target.stop_pending=false;
    out("{\"event\":\"steam_input_rumble_commands_complete\",\"dispatch_count\":4,\"final_zero_dispatched\":true,\"callback_verified\":false,\"physical_device_verified\":false}\n");
}
#endif
