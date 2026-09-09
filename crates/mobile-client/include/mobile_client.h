#ifndef BEX_MOBILE_CLIENT_H
#define BEX_MOBILE_CLIENT_H

#include <stddef.h>
#include <stdint.h>

/* iOS build artifact: <workspace>/target/<ios-target>/release/libmobile_client.a */

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque, non-reused ID. Zero denotes a failed connection. */
typedef uint64_t MobileClientHandle;

/*
 * Allocates an Ed25519 PKCS#8 document encoded as unpadded base64url.
 * Decode it and store the bytes in Keychain/Keystore; never log the result.
 * Every non-null returned string, including *error_out, is released with
 * mobile_client_string_free().
 */
char *mobile_client_generate_device_key(char **error_out);

/*
 * config_json uses camelCase fields:
 * relayUrl, relayToken, runnerId, hostIdentity, deviceName, pairingTicket (optional),
 * requestTimeoutMs. hostIdentity and pairingTicket are the
 * host-protocol base64url values. device_pkcs8 is secure-storage output.
 */
MobileClientHandle mobile_client_connect(
    const char *config_json,
    const uint8_t *device_pkcs8,
    size_t device_pkcs8_len,
    char **error_out);

/* Typed agent intent; shares operations with the native Mac client. */
char *mobile_client_agent_command(MobileClientHandle handle, const char *command_json, char **error_out);

/* Next raw notification or Host request, in wire order. NULL means empty unless
 * *error_out is set. Overflow/closure require reconnect and resynchronization. */
char *mobile_client_next_event(MobileClientHandle handle, char **error_out);

/* params: {direction:"upload",source,directory,fileName} or
 * {direction:"download",source,destination}. Download never overwrites.
 * Returns result JSON; file bytes use a separate SSH channel. */
char *mobile_client_transfer(MobileClientHandle handle, const char *params_json, char **error_out);

/* Retires the ID; outstanding calls retain the connection until they finish.
 * Repeated close is harmless and stale IDs fail without accessing freed memory. */
void mobile_client_close(MobileClientHandle handle);

/* Pure conversation presentation; no connection handle or source bodies needed.
 * Operations: turn (metadata + pending anchors), item (title metadata),
 * reconcile (pending/echoed client IDs). Result strings use string_free(). */
char *mobile_client_present_conversation(const char *request_json, char **error_out);

/* Stable event codes shared with ConversationEventKind in Kotlin. */
enum MobileConversationEvent {
    MOBILE_EVENT_UNKNOWN = 0, MOBILE_EVENT_TURN_STARTED = 1, MOBILE_EVENT_TURN_COMPLETED = 2,
    MOBILE_EVENT_ITEM_STARTED = 3, MOBILE_EVENT_ITEM_COMPLETED = 4, MOBILE_EVENT_AGENT_DELTA = 5,
    MOBILE_EVENT_REASONING_DELTA = 6, MOBILE_EVENT_REASONING_SUMMARY_DELTA = 7,
    MOBILE_EVENT_COMMAND_DELTA = 8, MOBILE_EVENT_FILE_DELTA = 9, MOBILE_EVENT_ERROR = 10,
    MOBILE_EVENT_REQUEST_STARTED = 11, MOBILE_EVENT_REQUEST_RESOLVED = 12,
    MOBILE_EVENT_THREAD_STATUS = 13, MOBILE_EVENT_GUARDIAN_REVIEW = 14
};
uint32_t mobile_client_classify_event(const char *method, int32_t is_request);
/* Status: 0 absent, 1 inProgress, 2 completed, 3 failed, 4 interrupted, 5 approved.
 * Item: 0 absent/unknown, 1 agentMessage, 2 reasoning, 3 commandExecution, 4 fileChange.
 * Input flags: 1 hasError, 2 willRetry, 4 emptyDelta, 8 current retryingError.
 * Result: low byte action, next byte status, bit 16 clearError.
 * Actions: 0 ignore, 1 threadStatus, 2 turn, 3 item, 4 removeItem, 5 append,
 *          6 error, 7 request, 8 resolveRequest. No strings require freeing. */
uint32_t mobile_client_conversation_transition(uint32_t kind, uint32_t status,
    uint32_t current_status, uint32_t item, uint32_t flags);

void mobile_client_string_free(char *value);

#ifdef __cplusplus
}
#endif

#endif
