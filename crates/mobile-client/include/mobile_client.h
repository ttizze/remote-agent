#ifndef BEX_MOBILE_CLIENT_H
#define BEX_MOBILE_CLIENT_H

#include <stddef.h>
#include <stdint.h>

/* iOS build artifact: <workspace>/target/<ios-target>/release/libmobile_client.a */

#ifdef __cplusplus
extern "C" {
#endif

typedef struct MobileClientHandle MobileClientHandle;

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
MobileClientHandle *mobile_client_connect(
    const char *config_json,
    const uint8_t *device_pkcs8,
    size_t device_pkcs8_len,
    char **error_out);

/* params_json must be one JSON value. The request is forwarded to Codex as-is. */
char *mobile_client_request(
    MobileClientHandle *handle,
    const char *method,
    const char *params_json,
    char **error_out);

/* Returns NULL when no notification is pending; check *error_out for errors. */
char *mobile_client_next_notification(MobileClientHandle *handle, char **error_out);

/*
 * Returns NULL when no Host-initiated request is pending. The returned JSON
 * is the raw request and contains an `id` that may be a JSON number or string.
 */
char *mobile_client_next_server_request(MobileClientHandle *handle, char **error_out);

/*
 * Answers a Host-initiated request. request_id_json must be the raw JSON
 * number/string from next_server_request. Return value is 1 on success and 0
 * on failure; on failure, *error_out receives a string when non-NULL.
 */
int mobile_client_respond_result(
    MobileClientHandle *handle,
    const char *request_id_json,
    const char *result_json,
    char **error_out);

int mobile_client_respond_error(
    MobileClientHandle *handle,
    const char *request_id_json,
    const char *error_json,
    char **error_out);

/* params: {direction:"upload",source,directory,fileName} or
 * {direction:"download",source,destination}. Download never overwrites.
 * Returns result JSON; file bytes use a separate SSH channel. */
char *mobile_client_transfer(MobileClientHandle *handle, const char *params_json, char **error_out);

/* Consumes handle; call exactly once after all request calls have returned. */
void mobile_client_close(MobileClientHandle *handle);

/* Pure conversation presentation; no connection handle or source bodies needed.
 * Operations: turn (metadata + pending anchors), item (title metadata),
 * reconcile (pending/echoed client IDs). Result strings use string_free(). */
char *mobile_client_present_conversation(const char *request_json, char **error_out);

void mobile_client_string_free(char *value);

#ifdef __cplusplus
}
#endif

#endif
