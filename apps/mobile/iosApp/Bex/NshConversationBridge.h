#ifndef BEX_NSH_CONVERSATION_BRIDGE_H
#define BEX_NSH_CONVERSATION_BRIDGE_H

#include <stdint.h>

#pragma clang assume_nonnull begin

typedef void (*BexNshEventCallback)(void * _Nullable, const uint8_t * _Nullable, intptr_t);

void * _Nullable (bex_nsh_conversation_create)(
    uint64_t session, uint64_t pane, const char *host,
    void *context, BexNshEventCallback callback);
void *bex_nsh_conversation_view(void *handle);
void bex_nsh_conversation_release(void *handle);
void bex_nsh_conversation_set_composer_inset(void *handle, float inset);

#pragma clang assume_nonnull end

#endif
